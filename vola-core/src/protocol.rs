//! Wire protocol between `vola-core` and a GUI client over WebSocket.
//!
//! Every frame is one JSON object. A client message carries `type` (and its fields beside it);
//! a server message carries `type` and, when it has a payload, `data`.

use serde::{Deserialize, Serialize};

use crate::agent::{TurnProgress, TurnReport};
use crate::config::{Config, ShellKind};
use crate::llm::ModelInfo;
use crate::memory::{NoteRow, SessionRow, Stats, StoredMessage, SummaryRow};
use crate::skills::SkillMeta;

/// Bumped when the shape of messages changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// What a GUI client asks for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Everything the GUI needs for its first screen.
    Status,
    Sessions {
        #[serde(default)]
        limit: Option<i64>,
    },
    Messages {
        session_id: i64,
    },
    NewSession,
    Resume {
        session_id: i64,
    },
    Chat {
        text: String,
    },
    Cancel,
    SetModel {
        model: String,
    },
    Models,
    Skills,
    Skill {
        name: String,
    },
    Notes {
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        limit: Option<i64>,
    },
    AddNote {
        content: String,
        #[serde(default)]
        pinned: bool,
        #[serde(default)]
        key: Option<String>,
    },
    DeleteNote {
        id: i64,
    },
    Compress,
    Tools,
    Stats,
    /// Run one slash command (`/help`, `/model`, `/session`, ...).
    Command {
        line: String,
    },
    DeleteSession {
        session_id: i64,
    },
}

/// What the server sends back.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready(ReadyView),
    Status(Box<StatusView>),
    Sessions {
        sessions: Vec<SessionRow>,
    },
    Messages {
        session_id: i64,
        messages: Vec<StoredMessage>,
        summary: Option<SummaryRow>,
    },
    Session {
        session_id: i64,
    },
    Model {
        model: String,
        num_ctx: u32,
    },
    Models {
        models: Vec<ModelInfo>,
    },
    Skills {
        skills: Vec<SkillView>,
    },
    Skill {
        skill: SkillView,
        body: String,
    },
    Notes {
        notes: Vec<NoteRow>,
    },
    Note {
        id: i64,
    },
    Deleted {
        id: i64,
        removed: bool,
    },
    Compressed {
        compacted: bool,
    },
    Tools {
        tools: Vec<ToolView>,
    },
    Stats {
        stats: Stats,
    },
    /// The text output of a slash command, plus flags the GUI acts on.
    CommandResult {
        command: String,
        text: String,
        quit: bool,
        reloaded: bool,
        session_changed: bool,
    },
    SessionDeleted {
        session_id: i64,
        removed: bool,
    },
    /// A turn started; one `turn_done` follows.
    TurnStarted {
        session_id: i64,
    },
    Progress(TurnProgress),
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        result: String,
        error: bool,
    },
    Notice {
        text: String,
    },
    TurnDone(TurnReport),
    /// A turn failed; the GUI ends the running turn.
    Error {
        message: String,
    },
    /// A request could not be served; the running turn (if any) is unaffected.
    RequestError {
        message: String,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadyView {
    pub protocol: u32,
    pub version: String,
    pub config: ConfigView,
    pub session_id: Option<i64>,
    pub agent_error: Option<String>,
}

/// The configuration a GUI may see: everything except credentials.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigView {
    pub home: String,
    pub provider_tag: String,
    pub provider_type: String,
    pub base_url: String,
    pub model: String,
    pub num_ctx: u32,
    pub temperature: f32,
    pub max_rounds: usize,
    pub max_tool_errors: usize,
    pub cmd_timeout_secs: u64,
    pub budget_chars: usize,
    pub db_path: String,
    pub skills_dir: String,
    pub assets_dir: String,
    pub scripts_dir: String,
    pub prompt_path: String,
    pub summarizer_path: String,
    pub shell: String,
    pub allow_dangerous: bool,
    pub verbose: bool,
}

impl ConfigView {
    pub fn from_config(cfg: &Config) -> ConfigView {
        ConfigView {
            home: cfg.home.display().to_string(),
            provider_tag: cfg.provider_tag.clone(),
            provider_type: cfg.provider_type.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            num_ctx: cfg.num_ctx,
            temperature: cfg.temperature,
            max_rounds: cfg.max_rounds,
            max_tool_errors: cfg.max_tool_errors,
            cmd_timeout_secs: cfg.cmd_timeout_secs,
            budget_chars: cfg.budget_chars,
            db_path: cfg.db_path.display().to_string(),
            skills_dir: cfg.skills_dir.display().to_string(),
            assets_dir: cfg.assets_dir.display().to_string(),
            scripts_dir: cfg.scripts_dir.display().to_string(),
            prompt_path: cfg.prompt_path.display().to_string(),
            summarizer_path: cfg.summarizer_path.display().to_string(),
            shell: shell_name(cfg.shell),
            allow_dangerous: cfg.allow_dangerous,
            verbose: cfg.verbose,
        }
    }
}

fn shell_name(shell: ShellKind) -> String {
    match shell {
        ShellKind::Bash => "bash",
        ShellKind::Cmd => "cmd",
        ShellKind::PowerShell => "powershell",
    }
    .to_string()
}

/// A skill as listed in the library (file metadata plus DB usage counters).
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
    pub name: String,
    pub description: String,
    pub tags: String,
    pub path: String,
    pub use_count: i64,
    pub updated_at: i64,
}

impl SkillView {
    pub fn new(meta: &SkillMeta, use_count: i64, updated_at: i64) -> SkillView {
        SkillView {
            name: meta.name.clone(),
            description: meta.description.clone(),
            tags: meta.tags.clone(),
            path: meta.path.display().to_string(),
            use_count,
            updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    pub name: String,
    pub description: String,
}

/// The full first-screen snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct StatusView {
    pub session_id: Option<i64>,
    pub agent_error: Option<String>,
    pub model: String,
    pub num_ctx: u32,
    pub budget_chars: usize,
    /// Size of the context the next request would send; `None` when it cannot be built.
    pub ctx_chars: Option<usize>,
    pub sessions: Vec<SessionRow>,
    pub skills: Vec<SkillView>,
    pub notes: Vec<NoteRow>,
    pub stats: Option<Stats>,
    pub tools: Vec<ToolView>,
}

impl ServerMessage {
    /// Serializes to the text of one WebSocket frame.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            serde_json::json!({ "type": "error", "data": { "message": e.to_string() } }).to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_requests() {
        let chat: ClientMessage =
            serde_json::from_str(r#"{"type":"chat","text":"hello"}"#).unwrap();
        assert!(matches!(chat, ClientMessage::Chat { text } if text == "hello"));

        let status: ClientMessage = serde_json::from_str(r#"{"type":"status"}"#).unwrap();
        assert!(matches!(status, ClientMessage::Status));

        let notes: ClientMessage =
            serde_json::from_str(r#"{"type":"notes","query":"rust"}"#).unwrap();
        assert!(matches!(notes, ClientMessage::Notes { query: Some(q), .. } if q == "rust"));

        let command: ClientMessage =
            serde_json::from_str(r#"{"type":"command","line":"/help"}"#).unwrap();
        assert!(matches!(command, ClientMessage::Command { line } if line == "/help"));

        let delete: ClientMessage =
            serde_json::from_str(r#"{"type":"delete_session","session_id":7}"#).unwrap();
        assert!(matches!(
            delete,
            ClientMessage::DeleteSession { session_id: 7 }
        ));

        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"nope"}"#).is_err());
    }

    #[test]
    fn serializes_server_messages() {
        let json = ServerMessage::Error {
            message: "boom".into(),
        }
        .to_json();
        assert_eq!(json, r#"{"type":"error","data":{"message":"boom"}}"#);

        let json = ServerMessage::Pong.to_json();
        assert_eq!(json, r#"{"type":"pong"}"#);

        let json = ServerMessage::Progress(TurnProgress {
            round: 2,
            ctx_chars: 100,
            budget_chars: 200,
            num_ctx: 4096,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_completion_tokens: 5,
        })
        .to_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "progress");
        assert_eq!(value["data"]["round"], 2);
    }
}
