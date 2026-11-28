//! The WebSocket API the GUI speaks: one `Agent` per connection, turns on worker threads,
//! structured events streamed back as they happen.
//!
//! The server is synchronous. A connection thread reads requests and answers them; a turn runs
//! on its own thread so `cancel` is still read while the model or a tool is working.

use anyhow::{anyhow, Context, Result};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::agent::{Agent, TurnEvent};
use crate::commands;
use crate::config::Config;
use crate::interrupt;
use crate::llm::{LlmClient, ModelInfo};
use crate::memory::{MemoryStore, SessionRow, Stats};
use crate::protocol::{
    ClientMessage, ConfigView, ReadyView, ServerMessage, SkillView, StatusView, ToolView,
    PROTOCOL_VERSION,
};
use crate::skills::{SkillMeta, Skills};
use crate::tools;
use crate::ws::{WsConnection, WsWriter};

/// Where the CLI serves and the GUI connects by default.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8765";

/// Serializes turns across connections: the cancellation flag is process-wide.
static TURN_LOCK: Mutex<()> = Mutex::new(());
/// True while a turn runs, so `cancel` does not leave a stale flag behind.
static TURN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Blocks forever, accepting GUI clients on `addr`.
pub fn serve(addr: &str, cfg: Config) -> Result<()> {
    let listener =
        TcpListener::bind(addr).with_context(|| format!("failed to listen on ws://{}", addr))?;
    serve_listener(listener, cfg)
}

/// Serves an already bound listener (useful for tests and embedding hosts).
pub fn serve_listener(listener: TcpListener, cfg: Config) -> Result<()> {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    eprintln!("vola-core WebSocket API listening on ws://{}", addr);
    eprintln!("Open vola-gui and connect to this address.");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let cfg = cfg.clone();
                thread::spawn(move || {
                    if let Err(e) = handle(stream, cfg) {
                        eprintln!("[ws] client disconnected: {:#}", e);
                    }
                });
            }
            Err(e) => eprintln!("[ws] accept failed: {}", e),
        }
    }
    Ok(())
}

struct ConnState {
    cfg: Config,
    agent: Option<Agent>,
    agent_error: Option<String>,
    session_id: Option<i64>,
}

fn handle(stream: std::net::TcpStream, cfg: Config) -> Result<()> {
    let mut conn = WsConnection::accept(stream)?;
    let writer = conn.writer();
    let state = Arc::new(Mutex::new(ConnState {
        cfg: cfg.clone(),
        agent: None,
        agent_error: None,
        session_id: None,
    }));
    let busy = Arc::new(AtomicBool::new(false));

    writer.send(&ServerMessage::Ready(ReadyView {
        protocol: PROTOCOL_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
        config: ConfigView::from_config(&cfg),
        session_id: None,
        agent_error: None,
    }))?;

    while let Some(text) = conn.read_text()? {
        let request: ClientMessage = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                writer.send(&ServerMessage::RequestError {
                    message: format!("invalid request: {}", e),
                })?;
                continue;
            }
        };

        match request {
            ClientMessage::Cancel => {
                let text = if TURN_ACTIVE.load(Ordering::SeqCst) {
                    interrupt::cancel();
                    "cancelling the current turn…".to_string()
                } else {
                    "no turn is running".to_string()
                };
                writer.send(&ServerMessage::Notice { text })?;
            }
            ClientMessage::Chat { text } => {
                if busy.swap(true, Ordering::SeqCst) {
                    writer.send(&ServerMessage::RequestError {
                        message: "a turn is already running".to_string(),
                    })?;
                    continue;
                }
                let state = Arc::clone(&state);
                let busy = Arc::clone(&busy);
                let writer = writer.clone();
                thread::spawn(move || {
                    run_turn(state, text, writer);
                    busy.store(false, Ordering::SeqCst);
                });
            }
            other => {
                // Never block the reader on the turn's state lock: `cancel` must stay
                // readable while a turn runs.
                let mut guard = match state.try_lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        writer.send(&ServerMessage::RequestError {
                            message: "the core is busy; try again when the current turn finishes"
                                .to_string(),
                        })?;
                        continue;
                    }
                };
                match handle_request(&mut guard, other) {
                    Ok(replies) => {
                        for reply in replies {
                            writer.send(&reply)?;
                        }
                    }
                    Err(e) => writer.send(&ServerMessage::RequestError {
                        message: format!("{:#}", e),
                    })?,
                }
            }
        }
    }
    Ok(())
}

fn run_turn(state: Arc<Mutex<ConnState>>, text: String, writer: WsWriter) {
    let _turn = TURN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    interrupt::begin_turn();
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => {
            let _ = writer.send(&ServerMessage::Error {
                message: "the connection state is poisoned".to_string(),
            });
            return;
        }
    };
    if let Err(e) = ensure_agent(&mut guard) {
        let _ = writer.send(&ServerMessage::Error {
            message: format!("{:#}", e),
        });
        interrupt::end_turn();
        return;
    }
    let session_id = guard.session_id.unwrap_or_default();
    TURN_ACTIVE.store(true, Ordering::SeqCst);
    let _ = writer.send(&ServerMessage::TurnStarted { session_id });

    let mut emit = |event: TurnEvent| {
        let message = match event {
            TurnEvent::Notice(text) => ServerMessage::Notice { text },
            TurnEvent::ToolCall { name, args } => ServerMessage::ToolCall { name, args },
            TurnEvent::ToolResult {
                name,
                result,
                error,
            } => ServerMessage::ToolResult {
                name,
                result,
                error,
            },
            TurnEvent::Progress(p) => ServerMessage::Progress(p),
        };
        let _ = writer.send(&message);
    };

    let result = guard
        .agent
        .as_mut()
        .expect("ensure_agent leaves an agent")
        .respond_events(&text, &mut emit);
    match result {
        Ok(report) => {
            let _ = writer.send(&ServerMessage::TurnDone(report));
        }
        Err(e) => {
            let _ = writer.send(&ServerMessage::Error {
                message: format!("{:#}", e),
            });
        }
    }
    TURN_ACTIVE.store(false, Ordering::SeqCst);
    interrupt::end_turn();
}

fn ensure_agent(state: &mut ConnState) -> Result<()> {
    if state.agent.is_some() {
        return Ok(());
    }
    match Agent::new(state.cfg.clone(), state.session_id) {
        Ok(agent) => {
            state.session_id = Some(agent.session_id);
            state.agent = Some(agent);
            state.agent_error = None;
            Ok(())
        }
        Err(e) => {
            let message = format!("{:#}", e);
            state.agent_error = Some(message.clone());
            Err(anyhow!(message))
        }
    }
}

fn with_store<T>(
    state: &mut ConnState,
    f: impl FnOnce(&mut MemoryStore) -> Result<T>,
) -> Result<T> {
    match state.agent.as_mut() {
        Some(agent) => f(&mut agent.store),
        None => {
            let mut store = MemoryStore::open(&state.cfg.db_path)?;
            f(&mut store)
        }
    }
}

fn handle_request(state: &mut ConnState, request: ClientMessage) -> Result<Vec<ServerMessage>> {
    match request {
        ClientMessage::Status => {
            // A first request may be what brings the agent up; a failure is reported in the
            // status rather than as a request error.
            let _ = ensure_agent(state);
            Ok(vec![ServerMessage::Status(Box::new(status_view(state)?))])
        }
        ClientMessage::Sessions { limit } => {
            let limit = limit.unwrap_or(50).clamp(1, 500);
            let sessions = with_store(state, |store| store.sessions(limit))?;
            Ok(vec![ServerMessage::Sessions { sessions }])
        }
        ClientMessage::Messages { session_id } => {
            let (messages, summary) = with_store(state, |store| {
                let summary = store.latest_summary(session_id)?;
                let after = summary.as_ref().map(|s| s.upto_message_id).unwrap_or(0);
                let messages = store.messages_after(session_id, after)?;
                Ok((messages, summary))
            })?;
            Ok(vec![ServerMessage::Messages {
                session_id,
                messages,
                summary,
            }])
        }
        ClientMessage::NewSession => {
            let model = current_model(state);
            let id = with_store(state, |store| store.new_session("", &model))?;
            state.session_id = Some(id);
            if let Some(agent) = state.agent.as_mut() {
                agent.session_id = id;
            }
            Ok(vec![ServerMessage::Session { session_id: id }])
        }
        ClientMessage::Resume { session_id } => {
            let title = with_store(state, |store| Ok(store.session_title(session_id)))?;
            if title.is_none() {
                return Err(anyhow!("no session #{}", session_id));
            }
            state.session_id = Some(session_id);
            if let Some(agent) = state.agent.as_mut() {
                agent.session_id = session_id;
            }
            Ok(vec![ServerMessage::Session { session_id }])
        }
        ClientMessage::SetModel { model } => {
            let model = model.trim().to_string();
            if model.is_empty() {
                return Err(anyhow!("the model name is empty"));
            }
            state.cfg.model = model.clone();
            if let Some(agent) = state.agent.as_mut() {
                agent.cfg.model = model.clone();
                agent.client = Arc::new(LlmClient::new(&agent.cfg));
            }
            Ok(vec![ServerMessage::Model {
                model,
                num_ctx: state.cfg.num_ctx,
            }])
        }
        ClientMessage::Models => {
            let models: Vec<ModelInfo> = match state.agent.as_ref() {
                Some(agent) => agent.client.list_models()?,
                None => LlmClient::new(&state.cfg).list_models()?,
            };
            Ok(vec![ServerMessage::Models { models }])
        }
        ClientMessage::Skills => Ok(vec![ServerMessage::Skills {
            skills: skills_view(state)?,
        }]),
        ClientMessage::Skill { name } => {
            let skills = Skills::new(state.cfg.skills_dir.clone());
            let (meta, body) = skills
                .load(&name)
                .ok_or_else(|| anyhow!("no skill named `{}`", name))?;
            let view = skill_view(state, &meta)?;
            Ok(vec![ServerMessage::Skill { skill: view, body }])
        }
        ClientMessage::Notes { query, limit } => {
            let query = query.unwrap_or_default();
            let limit = limit.unwrap_or(50).clamp(1, 500);
            let notes = with_store(state, |store| store.search_notes(&query, limit))?;
            Ok(vec![ServerMessage::Notes { notes }])
        }
        ClientMessage::AddNote {
            content,
            pinned,
            key,
        } => {
            if content.trim().is_empty() {
                return Err(anyhow!("the note is empty"));
            }
            let id = with_store(state, |store| {
                store.add_note(&content, "gui", pinned, key.as_deref())
            })?;
            Ok(vec![ServerMessage::Note { id }])
        }
        ClientMessage::DeleteNote { id } => {
            let removed = with_store(state, |store| store.delete_note(id))?;
            Ok(vec![ServerMessage::Deleted { id, removed }])
        }
        ClientMessage::Compress => {
            let agent = state
                .agent
                .as_mut()
                .ok_or_else(|| anyhow!("the model is not ready; fix the provider first"))?;
            let compacted = agent.force_compress()?;
            Ok(vec![ServerMessage::Compressed { compacted }])
        }
        ClientMessage::Tools => Ok(vec![ServerMessage::Tools {
            tools: tools_view(),
        }]),
        ClientMessage::Stats => {
            let stats: Stats = with_store(state, |store| store.stats())?;
            Ok(vec![ServerMessage::Stats { stats }])
        }
        ClientMessage::Complete { line } => Ok(vec![ServerMessage::Completions {
            line: line.clone(),
            candidates: commands::complete(&line),
        }]),
        ClientMessage::Command { line } => {
            if line.trim() == "/help" || line.trim() == "/?" {
                return Ok(vec![ServerMessage::CommandResult {
                    command: line,
                    text: commands::help_text().to_string(),
                    quit: false,
                    reloaded: false,
                    session_changed: false,
                }]);
            }
            let (output, new_session, new_model) = {
                let agent = state
                    .agent
                    .as_mut()
                    .ok_or_else(|| anyhow!("the model is not ready; fix the provider first"))?;
                let output = commands::execute(agent, &line)?;
                (output, agent.session_id, agent.cfg.model.clone())
            };
            state.cfg.model = new_model;
            let mut replies = vec![ServerMessage::CommandResult {
                command: line,
                text: output.text,
                quit: output.quit,
                reloaded: output.reloaded,
                session_changed: output.session_changed,
            }];
            if output.session_changed {
                state.session_id = Some(new_session);
                replies.push(ServerMessage::Session {
                    session_id: new_session,
                });
            }
            Ok(replies)
        }
        ClientMessage::DeleteSession { session_id } => {
            let removed = with_store(state, |store| store.delete_session(session_id))?;
            let mut replies = vec![ServerMessage::SessionDeleted {
                session_id,
                removed,
            }];
            if removed && state.session_id == Some(session_id) {
                // Switch to the most recent remaining session so deleting the active
                // (often the first) session visibly removes it; only create a fresh one
                // when none are left.
                let remaining = with_store(state, |store| store.sessions(1))?;
                let id = if let Some(row) = remaining.into_iter().next() {
                    row.id
                } else {
                    let model = current_model(state);
                    with_store(state, |store| store.new_session("", &model))?
                };
                state.session_id = Some(id);
                if let Some(agent) = state.agent.as_mut() {
                    agent.session_id = id;
                }
                replies.push(ServerMessage::Session { session_id: id });
            }
            Ok(replies)
        }
        ClientMessage::RenameSession { session_id, title } => {
            let title = title.trim().to_string();
            let renamed = with_store(state, |store| store.set_session_title(session_id, &title))?;
            Ok(vec![ServerMessage::SessionRenamed {
                session_id,
                renamed,
            }])
        }
        ClientMessage::PinSession { session_id, pinned } => {
            let updated = with_store(state, |store| store.set_session_pinned(session_id, pinned))?;
            Ok(vec![ServerMessage::SessionPinned {
                session_id,
                pinned,
                updated,
            }])
        }
        // Handled before this function.
        ClientMessage::Chat { .. } | ClientMessage::Cancel => Ok(vec![]),
    }
}

fn current_model(state: &ConnState) -> String {
    state
        .agent
        .as_ref()
        .map(|a| a.cfg.model.clone())
        .unwrap_or_else(|| state.cfg.model.clone())
}

fn skills_view(state: &mut ConnState) -> Result<Vec<SkillView>> {
    let skills = Skills::new(state.cfg.skills_dir.clone());
    let rows = with_store(state, |store| store.skills())?;
    Ok(skills
        .discover()
        .iter()
        .map(|meta| {
            let row = rows.iter().find(|r| r.name == meta.name);
            SkillView::new(
                meta,
                row.map(|r| r.use_count).unwrap_or(0),
                row.map(|r| r.updated_at).unwrap_or(0),
            )
        })
        .collect())
}

fn skill_view(state: &mut ConnState, meta: &SkillMeta) -> Result<SkillView> {
    let rows = with_store(state, |store| store.skills())?;
    let row = rows.iter().find(|r| r.name == meta.name);
    Ok(SkillView::new(
        meta,
        row.map(|r| r.use_count).unwrap_or(0),
        row.map(|r| r.updated_at).unwrap_or(0),
    ))
}

fn tools_view() -> Vec<ToolView> {
    tools::schemas()
        .iter()
        .map(|schema| {
            let function = &schema["function"];
            ToolView {
                name: function["name"].as_str().unwrap_or("?").to_string(),
                description: function["description"].as_str().unwrap_or("").to_string(),
            }
        })
        .collect()
}

fn status_view(state: &mut ConnState) -> Result<StatusView> {
    let sessions: Vec<SessionRow> = with_store(state, |store| store.sessions(50))?;
    let skills = skills_view(state)?;
    let notes = with_store(state, |store| store.search_notes("", 50))?;
    let stats = with_store(state, |store| store.stats()).ok();
    Ok(StatusView {
        session_id: state.session_id,
        agent_error: state.agent_error.clone(),
        model: current_model(state),
        num_ctx: state
            .agent
            .as_ref()
            .map(|a| a.cfg.num_ctx)
            .unwrap_or(state.cfg.num_ctx),
        budget_chars: state.cfg.budget_chars,
        ctx_chars: None,
        sessions,
        skills,
        notes,
        stats,
        tools: tools_view(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    struct TestWs {
        stream: TcpStream,
    }

    impl TestWs {
        fn connect(addr: SocketAddr) -> TestWs {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let request = format!(
                "GET / HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
                addr
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            let mut byte = [0u8; 1];
            while !response.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                response.push(byte[0]);
            }
            let text = String::from_utf8_lossy(&response);
            assert!(text.starts_with("HTTP/1.1 101"), "{}", text);
            assert!(text.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{}", text);
            TestWs { stream }
        }

        fn send(&mut self, json: &str) {
            let payload = json.as_bytes();
            let mut frame = vec![0x81];
            if payload.len() < 126 {
                frame.push(0x80 | payload.len() as u8);
            } else {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            }
            let mask = [0x12u8, 0x34, 0x56, 0x78];
            frame.extend_from_slice(&mask);
            for (i, byte) in payload.iter().enumerate() {
                frame.push(byte ^ mask[i % 4]);
            }
            self.stream.write_all(&frame).unwrap();
        }

        fn read(&mut self) -> serde_json::Value {
            let mut head = [0u8; 2];
            self.stream.read_exact(&mut head).unwrap();
            assert_eq!(head[0] & 0x0f, 1, "expected a text frame");
            let mut len = (head[1] & 0x7f) as usize;
            if len == 126 {
                let mut buf = [0u8; 2];
                self.stream.read_exact(&mut buf).unwrap();
                len = u16::from_be_bytes(buf) as usize;
            } else if len == 127 {
                let mut buf = [0u8; 8];
                self.stream.read_exact(&mut buf).unwrap();
                len = u64::from_be_bytes(buf) as usize;
            }
            let mut payload = vec![0u8; len];
            self.stream.read_exact(&mut payload).unwrap();
            serde_json::from_slice(&payload).unwrap()
        }

        fn request(&mut self, json: &str) -> serde_json::Value {
            self.send(json);
            self.read()
        }
    }

    fn test_config(dir: &std::path::Path) -> Config {
        let config_path = dir.join("config.json");
        let json = r#"{
            "database": "memory.db",
            "skills": "skills",
            "assets": "assets",
            "scripts": "scripts",
            "system": "SYSTEM.md",
            "memory": "MEMORY.md",
            "llm": {
                "default": "fake",
                "providers": [
                    {
                        "tag": "fake",
                        "type": "deepseek",
                        "config": {
                            "url": "http://127.0.0.1:1/v1",
                            "model": "test",
                            "api_key": "test"
                        }
                    }
                ]
            }
        }"#;
        std::fs::write(&config_path, json).unwrap();
        Config::load_from(Some(&config_path)).unwrap()
    }

    #[test]
    fn answers_the_gui_protocol() {
        let dir = std::env::temp_dir().join(format!(
            "vola-core-server-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_config(&dir);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let _ = serve_listener(listener, cfg);
        });

        let mut ws = TestWs::connect(addr);
        let ready = ws.read();
        assert_eq!(ready["type"], "ready");
        assert_eq!(
            ready["data"]["protocol"].as_u64(),
            Some(PROTOCOL_VERSION as u64)
        );

        let status = ws.request(r#"{"type":"status"}"#);
        assert_eq!(status["type"], "status");
        assert_eq!(status["data"]["model"], "test");
        assert!(status["data"]["agent_error"].is_null());
        assert_eq!(
            status["data"]["tools"].as_array().unwrap().len(),
            tools_view().len()
        );

        let note = ws.request(r#"{"type":"add_note","content":"remember the milk","pinned":true}"#);
        let id = note["data"]["id"].as_i64().unwrap();
        let notes = ws.request(r#"{"type":"notes","query":"milk"}"#);
        assert_eq!(notes["data"]["notes"].as_array().unwrap().len(), 1);

        let deleted = ws.request(&format!(r#"{{"type":"delete_note","id":{}}}"#, id));
        assert_eq!(deleted["type"], "deleted");
        assert_eq!(deleted["data"]["removed"], true);

        let session = ws.request(r#"{"type":"new_session"}"#);
        let session_id = session["data"]["session_id"].as_i64().unwrap();
        let messages = ws.request(&format!(
            r#"{{"type":"messages","session_id":{}}}"#,
            session_id
        ));
        assert_eq!(messages["type"], "messages");
        assert!(messages["data"]["messages"].as_array().unwrap().is_empty());

        let help = ws.request(r#"{"type":"command","line":"/help"}"#);
        assert_eq!(help["type"], "command_result");
        assert!(help["data"]["text"].as_str().unwrap().contains("/help"));

        let completions = ws.request(r#"{"type":"complete","line":"/se"}"#);
        assert_eq!(completions["type"], "completions");
        assert!(completions["data"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "/session"));

        let renamed = ws.request(&format!(
            r#"{{"type":"rename_session","session_id":{},"title":"renamed"}}"#,
            session_id
        ));
        assert_eq!(renamed["type"], "session_renamed");
        assert_eq!(renamed["data"]["renamed"], true);

        let pinned = ws.request(&format!(
            r#"{{"type":"pin_session","session_id":{},"pinned":true}}"#,
            session_id
        ));
        assert_eq!(pinned["type"], "session_pinned");
        assert_eq!(pinned["data"]["updated"], true);

        // Deleting the active session replies with `session_deleted` and a session to switch to.
        ws.send(&format!(
            r#"{{"type":"delete_session","session_id":{}}}"#,
            session_id
        ));
        let deleted = ws.read();
        assert_eq!(deleted["type"], "session_deleted");
        assert_eq!(deleted["data"]["removed"], true);
        let fresh = ws.read();
        assert_eq!(fresh["type"], "session");

        let cancel = ws.request(r#"{"type":"cancel"}"#);
        assert_eq!(cancel["type"], "notice");

        let bad = ws.request(r#"{"type":"nope"}"#);
        assert_eq!(bad["type"], "request_error");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
