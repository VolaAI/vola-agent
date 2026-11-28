//! Agent main loop: context budget and compression, tool dispatch, error counting, round cap, repeat-call detection.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::config::Config;
use crate::interrupt;
use crate::llm::{ChatResponse, LlmClient, Message};
use crate::memory::MemoryStore;
use crate::prompt;
use crate::scripts::Scripts;
use crate::skills::Skills;
use crate::tools::{self, ToolEnv};
use crate::util::{char_len, snippet};

#[derive(Debug, Clone, Serialize)]
pub struct TurnReport {
    pub text: String,
    pub rounds: usize,
    pub tool_calls: usize,
    pub elapsed_secs: f64,
    pub tokens_per_sec: Option<f64>,
    pub prompt_tokens: Option<u64>,
    /// Output tokens produced over the whole turn (summed over rounds).
    pub completion_tokens: u64,
    pub compressed: bool,
    pub ctx_chars: usize,
    /// The user interrupted the turn with Ctrl+C; `text` is empty.
    pub cancelled: bool,
}

/// Live numbers reported while a turn runs, for the REPL status line and the GUI.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TurnProgress {
    /// The model round this snapshot belongs to (1-based).
    pub round: usize,
    pub ctx_chars: usize,
    pub budget_chars: usize,
    pub num_ctx: u32,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_completion_tokens: u64,
}

/// What happens during a turn, besides the final report.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// A human-readable notice (a guard fired, history was compacted).
    Notice(String),
    /// The model asked for a tool call.
    ToolCall { name: String, args: String },
    /// A tool call finished; `error` marks a failed or rejected call.
    ToolResult {
        name: String,
        result: String,
        error: bool,
    },
    /// Live round, context and token numbers.
    Progress(TurnProgress),
}

pub struct Agent {
    pub cfg: Config,
    pub client: Arc<LlmClient>,
    pub store: MemoryStore,
    pub skills: Skills,
    pub scripts: Scripts,
    pub session_id: i64,
    /// Compressor prompt, loaded from agent/MEMORY.md at startup (see `prompt::load_summarizer`).
    pub summarizer: String,
}

impl Agent {
    pub fn new(cfg: Config, session_id: Option<i64>) -> Result<Agent> {
        let client = Arc::new(LlmClient::new(&cfg));
        // Fail before a session row is written: a missing server or model should not leave
        // an empty session behind.
        client.ensure_ready()?;
        let store = MemoryStore::open(&cfg.db_path)?;
        let skills = Skills::new(cfg.skills_dir.clone());
        let scripts = Scripts::new(cfg.scripts_dir.clone());
        let _ = scripts.write_index();
        let mut store = store;
        let session_id = match session_id {
            Some(id) => id,
            None => store.new_session("", &cfg.model)?,
        };
        // Index skills into the DB so the agent knows which skills it owns
        for m in skills.discover() {
            store.upsert_skill(&m.name, &m.description, &m.path.to_string_lossy())?;
        }
        let summarizer = prompt::load_summarizer(&cfg);
        Ok(Agent {
            cfg,
            client,
            store,
            skills,
            scripts,
            session_id,
            summarizer,
        })
    }

    /// Reload prompts, skills, scripts and the LLM client from disk/config.
    pub fn reload(&mut self) -> Result<()> {
        self.summarizer = prompt::load_summarizer(&self.cfg);
        self.client = Arc::new(LlmClient::new(&self.cfg));
        for m in self.skills.discover() {
            self.store
                .upsert_skill(&m.name, &m.description, &m.path.to_string_lossy())?;
        }
        self.scripts.write_index()?;
        Ok(())
    }

    /// Handle one user turn and return the final answer.
    pub fn respond<F>(&mut self, input: &str, log: F) -> Result<TurnReport>
    where
        F: FnMut(&str),
    {
        self.respond_with(input, log, &mut |_| {})
    }

    /// Handle one turn, reporting live progress (round, context size, token usage) as it goes.
    pub fn respond_with<F>(
        &mut self,
        input: &str,
        mut log: F,
        progress: &mut dyn FnMut(&TurnProgress),
    ) -> Result<TurnReport>
    where
        F: FnMut(&str),
    {
        self.respond_events(input, &mut |event| match event {
            TurnEvent::Notice(text) => log(&text),
            TurnEvent::ToolCall { name, args } => log(&format!("→ {}({})", name, args)),
            TurnEvent::ToolResult {
                name: _,
                result,
                error,
            } => {
                let shown = snippet(&result.replace('\n', " "), if error { 200 } else { 400 });
                log(&format!("{} {}", if error { "✗" } else { "✓" }, shown));
            }
            TurnEvent::Progress(p) => progress(&p),
        })
    }

    /// Handle one turn, emitting structured events for a UI (the REPL maps them back to text).
    pub fn respond_events(
        &mut self,
        input: &str,
        emit: &mut dyn FnMut(TurnEvent),
    ) -> Result<TurnReport> {
        let started = Instant::now();
        self.store
            .append_message(self.session_id, "user", input, None, None, None)?;
        if self
            .store
            .session_title(self.session_id)
            .map(|t| t.is_empty())
            .unwrap_or(true)
        {
            let title = snippet(&input.replace('\n', " "), 40);
            self.store.set_title_if_empty(self.session_id, &title)?;
        }

        let schemas = tools::schemas();
        let mut rounds = 0usize;
        let mut total_calls = 0usize;
        let mut consecutive_errors = 0usize;
        let mut compressed_any = false;
        let mut last_rate: Option<f64> = None;
        let mut last_prompt_tokens: Option<u64> = None;
        let mut last_completion_tokens: Option<u64> = None;
        let mut total_completion_tokens = 0u64;
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut done: Vec<String> = Vec::new();
        let mut last_err = String::new();
        let mut last_ctx_chars = 0usize;

        loop {
            if interrupt::cancelled() {
                return Ok(cancelled_turn(
                    rounds,
                    total_calls,
                    started,
                    last_ctx_chars,
                    last_rate,
                    last_prompt_tokens,
                    compressed_any,
                ));
            }
            rounds += 1;
            let (messages, compressed) = match self.build_context() {
                Ok(v) => v,
                Err(_) if interrupt::cancelled() => {
                    return Ok(cancelled_turn(
                        rounds,
                        total_calls,
                        started,
                        last_ctx_chars,
                        last_rate,
                        last_prompt_tokens,
                        compressed_any,
                    ));
                }
                Err(e) => return Err(e),
            };
            compressed_any |= compressed;
            let ctx_chars = messages.iter().map(|m| m.weight()).sum::<usize>();
            last_ctx_chars = ctx_chars;
            emit_progress(
                emit,
                rounds,
                ctx_chars,
                self.cfg.budget_chars,
                self.cfg.num_ctx,
                last_prompt_tokens,
                last_completion_tokens,
                total_completion_tokens,
            );

            let resp = match self.chat_interruptible(&messages, Some(&schemas)) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Ok(cancelled_turn(
                        rounds,
                        total_calls,
                        started,
                        ctx_chars,
                        last_rate,
                        last_prompt_tokens,
                        compressed_any,
                    ));
                }
                Err(_) if interrupt::cancelled() => {
                    return Ok(cancelled_turn(
                        rounds,
                        total_calls,
                        started,
                        ctx_chars,
                        last_rate,
                        last_prompt_tokens,
                        compressed_any,
                    ));
                }
                Err(e) => return Err(e),
            };
            last_rate = resp.tokens_per_sec().or(last_rate);
            last_prompt_tokens = resp.prompt_tokens().or(last_prompt_tokens);
            last_completion_tokens = resp.completion_tokens().or(last_completion_tokens);
            if let Some(c) = resp.completion_tokens() {
                total_completion_tokens += c;
            }
            emit_progress(
                emit,
                rounds,
                ctx_chars,
                self.cfg.budget_chars,
                self.cfg.num_ctx,
                last_prompt_tokens,
                last_completion_tokens,
                total_completion_tokens,
            );

            let content = resp.content();
            let mut calls = resp.calls();
            for (index, call) in calls.iter_mut().enumerate() {
                if call.id.as_deref().unwrap_or("").is_empty() {
                    call.id = Some(format!("call_{}_{}", rounds, index));
                }
            }
            if interrupt::cancelled() {
                return Ok(cancelled_turn(
                    rounds,
                    total_calls,
                    started,
                    ctx_chars,
                    last_rate,
                    last_prompt_tokens,
                    compressed_any,
                ));
            }
            // Important: never echo `thinking` back; store content + tool_calls only.
            // Skip a completely empty assistant turn so it cannot pollute later context.
            if !content.trim().is_empty() || !calls.is_empty() {
                self.store.append_message(
                    self.session_id,
                    "assistant",
                    &content,
                    None,
                    if calls.is_empty() { None } else { Some(&calls) },
                    None,
                )?;
            }

            if calls.is_empty() {
                let text = if !content.trim().is_empty() {
                    content
                } else if !done.is_empty() {
                    // The tools already ran but the model returned no text. Ask once for a
                    // direct answer, with tools disabled, instead of showing only raw results.
                    let (mut ctx, _) = match self.build_context() {
                        Ok(v) => v,
                        Err(_) if interrupt::cancelled() => {
                            return Ok(cancelled_turn(
                                rounds,
                                total_calls,
                                started,
                                ctx_chars,
                                last_rate,
                                last_prompt_tokens,
                                compressed_any,
                            ));
                        }
                        Err(e) => return Err(e),
                    };
                    ctx.push(Message::user(
                        "The tool calls above have already run. Answer the user's question directly now, in the user's language, using the tool output above. Do not call any more tools.",
                    ));
                    let follow = match self.chat_interruptible(&ctx, None) {
                        Ok(Some(r)) => r,
                        Ok(None) => {
                            return Ok(cancelled_turn(
                                rounds,
                                total_calls,
                                started,
                                ctx_chars,
                                last_rate,
                                last_prompt_tokens,
                                compressed_any,
                            ));
                        }
                        Err(_) if interrupt::cancelled() => {
                            return Ok(cancelled_turn(
                                rounds,
                                total_calls,
                                started,
                                ctx_chars,
                                last_rate,
                                last_prompt_tokens,
                                compressed_any,
                            ));
                        }
                        Err(e) => return Err(e),
                    };
                    last_rate = follow.tokens_per_sec().or(last_rate);
                    last_prompt_tokens = follow.prompt_tokens().or(last_prompt_tokens);
                    last_completion_tokens = follow.completion_tokens().or(last_completion_tokens);
                    if let Some(c) = follow.completion_tokens() {
                        total_completion_tokens += c;
                    }
                    emit_progress(
                        emit,
                        rounds,
                        ctx_chars,
                        self.cfg.budget_chars,
                        self.cfg.num_ctx,
                        last_prompt_tokens,
                        last_completion_tokens,
                        total_completion_tokens,
                    );
                    let answer = follow.content().trim().to_string();
                    if answer.is_empty() {
                        format!(
                            "Executed {} tool calls, results below:\n{}",
                            done.len(),
                            done.join("\n")
                        )
                    } else {
                        self.store.append_message(
                            self.session_id,
                            "assistant",
                            &answer,
                            None,
                            None,
                            None,
                        )?;
                        answer
                    }
                } else if !done.is_empty() {
                    format!(
                        "Executed {} tool calls, results below:\n{}",
                        done.len(),
                        done.join("\n")
                    )
                } else {
                    "(The model produced no output. Try rephrasing, or check /ctx to see whether the context is full.)".to_string()
                };
                return Ok(TurnReport {
                    text,
                    rounds,
                    tool_calls: total_calls,
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    tokens_per_sec: last_rate,
                    prompt_tokens: last_prompt_tokens,
                    completion_tokens: total_completion_tokens,
                    compressed: compressed_any,
                    ctx_chars,
                    cancelled: false,
                });
            }

            if rounds > self.cfg.max_rounds {
                emit(TurnEvent::Notice(format!(
                    "⚠ tool round limit {} reached, asking the model to answer directly",
                    self.cfg.max_rounds
                )));
                let mut last = messages.clone();
                last.push(Message::user(
                    "The tool-call round limit has been reached. Stop calling tools and answer directly, in the user's language, from the information above; state what remains unverified.",
                ));
                let resp = match self.chat_interruptible(&last, None) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        return Ok(cancelled_turn(
                            rounds,
                            total_calls,
                            started,
                            ctx_chars,
                            last_rate,
                            last_prompt_tokens,
                            compressed_any,
                        ));
                    }
                    Err(_) if interrupt::cancelled() => {
                        return Ok(cancelled_turn(
                            rounds,
                            total_calls,
                            started,
                            ctx_chars,
                            last_rate,
                            last_prompt_tokens,
                            compressed_any,
                        ));
                    }
                    Err(e) => return Err(e),
                };
                last_rate = resp.tokens_per_sec().or(last_rate);
                last_prompt_tokens = resp.prompt_tokens().or(last_prompt_tokens);
                last_completion_tokens = resp.completion_tokens().or(last_completion_tokens);
                if let Some(c) = resp.completion_tokens() {
                    total_completion_tokens += c;
                }
                emit_progress(
                    emit,
                    rounds,
                    ctx_chars,
                    self.cfg.budget_chars,
                    self.cfg.num_ctx,
                    last_prompt_tokens,
                    last_completion_tokens,
                    total_completion_tokens,
                );
                let text = if resp.content().trim().is_empty() {
                    "(round limit reached and the model gave no conclusion; narrow the task and retry)".to_string()
                } else {
                    resp.content()
                };
                self.store
                    .append_message(self.session_id, "assistant", &text, None, None, None)?;
                return Ok(TurnReport {
                    text,
                    rounds,
                    tool_calls: total_calls,
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    tokens_per_sec: last_rate,
                    prompt_tokens: last_prompt_tokens,
                    completion_tokens: total_completion_tokens,
                    compressed: compressed_any,
                    ctx_chars,
                    cancelled: false,
                });
            }

            for (index, call) in calls.iter().enumerate() {
                if interrupt::cancelled() {
                    self.append_cancelled_tools(&calls[index..])?;
                    return Ok(cancelled_turn(
                        rounds,
                        total_calls,
                        started,
                        ctx_chars,
                        last_rate,
                        last_prompt_tokens,
                        compressed_any,
                    ));
                }
                total_calls += 1;
                let tool_call_id = call.id.clone();
                let name = call.function.name.clone();
                let args = normalize_args(&call.function.arguments);
                let preview = snippet(&args.to_string(), 160);
                emit(TurnEvent::ToolCall {
                    name: name.clone(),
                    args: preview.clone(),
                });

                let key = format!("{}|{}", name, args);
                let times = *seen.get(&key).unwrap_or(&0);
                if times >= 2 {
                    let warn = format!(
                        "[Tool error] the same call has repeated {} times and is treated as a loop; change approach or answer directly.",
                        times
                    );
                    emit(TurnEvent::ToolResult {
                        name: name.clone(),
                        result: warn.clone(),
                        error: true,
                    });
                    self.store.append_message(
                        self.session_id,
                        "tool",
                        &warn,
                        Some(&name),
                        None,
                        tool_call_id.as_deref(),
                    )?;
                    consecutive_errors += 1;
                    if interrupt::cancelled() {
                        self.append_cancelled_tools(&calls[index + 1..])?;
                        return Ok(cancelled_turn(
                            rounds,
                            total_calls,
                            started,
                            ctx_chars,
                            last_rate,
                            last_prompt_tokens,
                            compressed_any,
                        ));
                    }
                    continue;
                }
                seen.insert(key, times + 1);

                let mut env = ToolEnv {
                    cfg: &self.cfg,
                    store: &mut self.store,
                    skills: &self.skills,
                    scripts: &self.scripts,
                };
                let result = tools::execute(&mut env, &name, &args);
                let is_err =
                    result.starts_with("[Tool error]") || result.starts_with("[Unknown tool]");
                if is_err {
                    consecutive_errors += 1;
                    last_err = snippet(&result, 160);
                    emit(TurnEvent::ToolResult {
                        name: name.clone(),
                        result: result.clone(),
                        error: true,
                    });
                } else {
                    consecutive_errors = 0;
                    done.push(format!(
                        "- {}: {}",
                        name,
                        snippet(&result.replace('\n', " "), 400)
                    ));
                    emit(TurnEvent::ToolResult {
                        name: name.clone(),
                        result: result.clone(),
                        error: false,
                    });
                }
                self.store.append_message(
                    self.session_id,
                    "tool",
                    &result,
                    Some(&name),
                    None,
                    tool_call_id.as_deref(),
                )?;
                if interrupt::cancelled() {
                    self.append_cancelled_tools(&calls[index + 1..])?;
                    return Ok(cancelled_turn(
                        rounds,
                        total_calls,
                        started,
                        ctx_chars,
                        last_rate,
                        last_prompt_tokens,
                        compressed_any,
                    ));
                }
            }

            if consecutive_errors >= self.cfg.max_tool_errors {
                let msg = format!(
                    "{} consecutive tool calls failed or were flagged as loops; stopping proactively. Last error: {}\nCheck the command syntax and paths, or ask the user to confirm the environment.",
                    consecutive_errors, last_err
                );
                self.store
                    .append_message(self.session_id, "assistant", &msg, None, None, None)?;
                return Ok(TurnReport {
                    text: msg,
                    rounds,
                    tool_calls: total_calls,
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    tokens_per_sec: last_rate,
                    prompt_tokens: last_prompt_tokens,
                    completion_tokens: total_completion_tokens,
                    compressed: compressed_any,
                    ctx_chars,
                    cancelled: false,
                });
            }
        }
    }

    /// Build the full message sequence sent to the model; compress history when needed.
    fn build_context(&mut self) -> Result<(Vec<Message>, bool)> {
        let summary = self.store.latest_summary(self.session_id)?;
        let after = summary.as_ref().map(|s| s.upto_message_id).unwrap_or(0);
        let stored = self.store.messages_after(self.session_id, after)?;
        let mut summary_text = summary.map(|s| s.text).unwrap_or_default();
        let mut msgs: Vec<(i64, Message)> =
            stored.into_iter().map(|m| (m.id, from_stored(m))).collect();
        let mut compressed = false;

        let pinned = self.store.pinned_notes(12)?;
        let metas = self.skills.discover();
        // Keep the DB index and the message count in sync
        for m in &metas {
            let _ = self
                .store
                .upsert_skill(&m.name, &m.description, &m.path.to_string_lossy());
        }
        let skills_index = self.skills.render_index(&metas);
        let script_metas = self.scripts.discover();
        let scripts_index = self.scripts.render_index(&script_metas);
        let agent_md = prompt::load_agent_md(&self.cfg)?;

        loop {
            let sys_len = char_len(&agent_md)
                + char_len(&skills_index)
                + char_len(&scripts_index)
                + summary_text.chars().count()
                + pinned
                    .iter()
                    .map(|n| char_len(&n.content) + 12)
                    .sum::<usize>()
                + 1200;
            let msgs_len: usize = msgs.iter().map(|(_, m)| m.weight()).sum();
            if sys_len + msgs_len <= self.cfg.budget_chars || msgs.len() <= 3 {
                let full = prompt::build(
                    &self.cfg,
                    &agent_md,
                    &skills_index,
                    &scripts_index,
                    &pinned,
                    if summary_text.is_empty() {
                        None
                    } else {
                        Some(&summary_text)
                    },
                    self.session_id,
                );
                let mut out = vec![Message::system(full)];
                out.extend(msgs.iter().map(|(_, m)| m.clone()));
                return Ok((out, compressed));
            }

            let cut = choose_cut(&msgs);
            let upto_id = msgs[cut - 1].0;
            let chunk: Vec<&(i64, Message)> = msgs[..cut].iter().collect();
            let new_summary = match self.summarize(&summary_text, &chunk) {
                Ok(s) => s,
                Err(_) if interrupt::cancelled() => {
                    return Err(anyhow!("the user cancelled the summary request"));
                }
                Err(e) => format!(
                    "(auto-compaction: summary generation failed: {}) earlier messages: {}; last content: {}",
                    e,
                    chunk.len(),
                    snippet(&chunk.last().map(|(_, m)| m.content.clone()).unwrap_or_default(), 300)
                ),
            };
            self.store
                .add_summary(self.session_id, upto_id, &new_summary)?;
            summary_text = new_summary;
            msgs = msgs.split_off(cut);
            compressed = true;
        }
    }

    fn summarize(&self, prev: &str, chunk: &[&(i64, Message)]) -> Result<String> {
        let mut transcript = String::new();
        if !prev.trim().is_empty() {
            transcript.push_str(&format!("[Existing summary]\n{}\n\n", prev.trim()));
        }
        transcript.push_str("[New conversation]\n");
        for (_, m) in chunk {
            let body = snippet(&m.content, 500);
            match m.role.as_str() {
                "user" => transcript.push_str(&format!("User: {}\n", body)),
                "assistant" => {
                    if m.tool_calls
                        .as_ref()
                        .map(|c| !c.is_empty())
                        .unwrap_or(false)
                    {
                        let names: Vec<String> = m
                            .tool_calls
                            .as_ref()
                            .unwrap()
                            .iter()
                            .map(|c| {
                                format!(
                                    "{}({})",
                                    c.function.name,
                                    snippet(&c.function.arguments.to_string(), 80)
                                )
                            })
                            .collect();
                        transcript
                            .push_str(&format!("Assistant (tool calls): {}\n", names.join("; ")));
                    } else {
                        transcript.push_str(&format!("Assistant: {}\n", body));
                    }
                }
                "tool" => transcript.push_str(&format!(
                    "Tool result ({}): {}\n",
                    m.tool_name.clone().unwrap_or_else(|| "?".into()),
                    body
                )),
                other => transcript.push_str(&format!("{}: {}\n", other, body)),
            }
        }
        let messages = vec![
            Message::system(self.summarizer.clone()),
            Message::user(format!(
                "Summarize the following conversation:\n\n{}",
                transcript
            )),
        ];
        let resp: ChatResponse = match self.chat_interruptible(&messages, None) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(anyhow!("the summary request was cancelled by the user")),
            Err(e) => return Err(e),
        };
        let text = resp.content().trim().to_string();
        if text.is_empty() {
            return Err(anyhow!("summary is empty"));
        }
        Ok(text)
    }

    pub fn context_chars(&mut self) -> Result<usize> {
        let (msgs, _) = self.build_context()?;
        Ok(msgs.iter().map(|m| m.weight()).sum())
    }

    pub fn force_compress(&mut self) -> Result<bool> {
        let summary = self.store.latest_summary(self.session_id)?;
        let after = summary.as_ref().map(|s| s.upto_message_id).unwrap_or(0);
        let msgs: Vec<(i64, Message)> = self
            .store
            .messages_after(self.session_id, after)?
            .into_iter()
            .map(|m| (m.id, from_stored(m)))
            .collect();
        if msgs.len() <= 2 {
            return Ok(false);
        }
        let summary_text = summary.map(|s| s.text).unwrap_or_default();
        let cut = choose_cut(&msgs);
        let chunk: Vec<&(i64, Message)> = msgs[..cut].iter().collect();
        let upto_id = msgs[cut - 1].0;
        let new_summary = self.summarize(&summary_text, &chunk)?;
        self.store
            .add_summary(self.session_id, upto_id, &new_summary)?;
        Ok(true)
    }

    /// Append one `tool` reply per call that was never run because the user cancelled,
    /// so an assistant `tool_calls` message never loses its matching results.
    fn append_cancelled_tools(&mut self, calls: &[crate::llm::ToolCall]) -> Result<()> {
        for call in calls {
            self.store.append_message(
                self.session_id,
                "tool",
                "[cancelled] the user stopped this turn; the tool was not run.",
                Some(&call.function.name),
                None,
                call.id.as_deref(),
            )?;
        }
        Ok(())
    }

    /// One model request on a helper thread, so the REPL can stop waiting on Ctrl+C instead
    /// of freezing until the request finishes. A cancelled request is left to finish in the
    /// background and its result is dropped without touching the session.
    fn chat_interruptible(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
    ) -> Result<Option<ChatResponse>> {
        if interrupt::cancelled() {
            return Ok(None);
        }
        let client = Arc::clone(&self.client);
        let messages = messages.to_vec();
        let tools = tools.map(|t| t.to_vec());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(client.chat(&messages, tools.as_deref()));
        });
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(result) => {
                    if interrupt::cancelled() {
                        return Ok(None);
                    }
                    return result.map(Some);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if interrupt::cancelled() {
                        return Ok(None);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("the model request thread exited without a result"));
                }
            }
        }
    }
}

fn emit_progress(
    emit: &mut dyn FnMut(TurnEvent),
    round: usize,
    ctx_chars: usize,
    budget_chars: usize,
    num_ctx: u32,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_completion_tokens: u64,
) {
    emit(TurnEvent::Progress(TurnProgress {
        round,
        ctx_chars,
        budget_chars,
        num_ctx,
        prompt_tokens,
        completion_tokens,
        total_completion_tokens,
    }));
}

fn cancelled_turn(
    rounds: usize,
    tool_calls: usize,
    started: Instant,
    ctx_chars: usize,
    rate: Option<f64>,
    prompt_tokens: Option<u64>,
    compressed: bool,
) -> TurnReport {
    TurnReport {
        text: String::new(),
        rounds,
        tool_calls,
        elapsed_secs: started.elapsed().as_secs_f64(),
        tokens_per_sec: rate,
        prompt_tokens,
        completion_tokens: 0,
        compressed,
        ctx_chars,
        cancelled: true,
    }
}

fn from_stored(m: crate::memory::StoredMessage) -> Message {
    Message {
        role: m.role,
        content: m.content,
        tool_calls: m.tool_calls,
        tool_name: m.tool_name,
        tool_call_id: m.tool_call_id,
    }
}

/// Pick the compression cut point: prefer a boundary right before a `user` message, so an assistant `tool_calls` message and its `tool` results are never split apart.
fn choose_cut(msgs: &[(i64, Message)]) -> usize {
    let mid = msgs.len() / 2;
    if let Some(idx) = msgs
        .iter()
        .enumerate()
        .skip(1)
        .take(mid.max(1))
        .rev()
        .find(|(_, (_, m))| m.role == "user")
        .map(|(i, _)| i)
    {
        return idx;
    }
    mid.max(1)
}

/// Tolerate models that send arguments as a string or use aliased parameter names.
fn normalize_args(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                parsed
            } else {
                serde_json::json!({ "cmd": s })
            }
        }
        Value::Null => serde_json::json!({}),
        other => other.clone(),
    }
}
