//! Agent main loop: context budget and compression, tool dispatch, error counting, round cap, repeat-call detection.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

use crate::config::Config;
use crate::llm::{ChatResponse, LlmClient, Message};
use crate::memory::MemoryStore;
use crate::prompt;
use crate::skills::Skills;
use crate::tools::{self, ToolEnv};
use crate::util::{char_len, snippet};

pub struct TurnReport {
    pub text: String,
    pub rounds: usize,
    pub tool_calls: usize,
    pub elapsed_secs: f64,
    pub tokens_per_sec: Option<f64>,
    pub prompt_tokens: Option<u64>,
    pub compressed: bool,
    pub ctx_chars: usize,
}

pub struct Agent {
    pub cfg: Config,
    pub client: LlmClient,
    pub store: MemoryStore,
    pub skills: Skills,
    pub session_id: i64,
    /// Compressor prompt, loaded from agent/MEMORY.md at startup (see `prompt::load_summarizer`).
    pub summarizer: String,
}

impl Agent {
    pub fn new(cfg: Config, session_id: Option<i64>) -> Result<Agent> {
        let client = LlmClient::new(&cfg);
        // Fail before a session row is written: a missing server or model should not leave
        // an empty session behind.
        client.ensure_ready()?;
        let store = MemoryStore::open(&cfg.db_path)?;
        let skills = Skills::new(cfg.skills_dir.clone());
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
            session_id,
            summarizer,
        })
    }

    /// Handle one user turn and return the final answer.
    pub fn respond<F>(&mut self, input: &str, mut log: F) -> Result<TurnReport>
    where
        F: FnMut(&str),
    {
        let started = Instant::now();
        self.store
            .append_message(self.session_id, "user", input, None, None, None)?;
        if self.store.session_title(self.session_id).map(|t| t.is_empty()).unwrap_or(true) {
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
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut done: Vec<String> = Vec::new();
        let mut last_err = String::new();

        loop {
            rounds += 1;
            let (messages, compressed) = self.build_context()?;
            compressed_any |= compressed;
            let ctx_chars = messages
                .iter()
                .map(|m| m.weight())
                .sum::<usize>();

            let resp = self.client.chat(&messages, Some(&schemas))?;
            last_rate = resp.tokens_per_sec().or(last_rate);
            last_prompt_tokens = resp.prompt_tokens().or(last_prompt_tokens);

            let content = resp.content();
            let mut calls = resp.calls();
            for (index, call) in calls.iter_mut().enumerate() {
                if call.id.as_deref().unwrap_or("").is_empty() {
                    call.id = Some(format!("call_{}_{}", rounds, index));
                }
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
                    let (mut ctx, _) = self.build_context()?;
                    ctx.push(Message::user(
                        "The tool calls above have already run. Answer the user's question directly now, in the user's language, using the tool output above. Do not call any more tools.",
                    ));
                    let follow = self.client.chat(&ctx, None)?;
                    last_rate = follow.tokens_per_sec().or(last_rate);
                    last_prompt_tokens = follow.prompt_tokens().or(last_prompt_tokens);
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
                    compressed: compressed_any,
                    ctx_chars,
                });
            }

            if rounds > self.cfg.max_rounds {
                log(&format!(
                    "⚠ tool round limit {} reached, asking the model to answer directly",
                    self.cfg.max_rounds
                ));
                let mut last = messages.clone();
                last.push(Message::user(
                    "The tool-call round limit has been reached. Stop calling tools and answer directly, in the user's language, from the information above; state what remains unverified.",
                ));
                let resp = self.client.chat(&last, None)?;
                let text = if resp.content().trim().is_empty() {
                    "(round limit reached and the model gave no conclusion; narrow the task and retry)".to_string()
                } else {
                    resp.content()
                };
                self.store.append_message(
                    self.session_id,
                    "assistant",
                    &text,
                    None,
                    None,
                    None,
                )?;
                return Ok(TurnReport {
                    text,
                    rounds,
                    tool_calls: total_calls,
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    tokens_per_sec: last_rate,
                    prompt_tokens: last_prompt_tokens,
                    compressed: compressed_any,
                    ctx_chars,
                });
            }

            for call in calls {
                total_calls += 1;
                let tool_call_id = call.id.clone();
                let name = call.function.name.clone();
                let args = normalize_args(&call.function.arguments);
                let preview = snippet(&args.to_string(), 160);
                log(&format!("→ {}({})", name, preview));

                let key = format!("{}|{}", name, args);
                let times = *seen.get(&key).unwrap_or(&0);
                if times >= 2 {
                    let warn = format!(
                        "[Tool error] the same call has repeated {} times and is treated as a loop; change approach or answer directly.",
                        times
                    );
                    log(&format!("✗ {}", warn));
                    self.store.append_message(
                        self.session_id,
                        "tool",
                        &warn,
                        Some(&name),
                        None,
                        tool_call_id.as_deref(),
                    )?;
                    consecutive_errors += 1;
                    continue;
                }
                seen.insert(key, times + 1);

                let mut env = ToolEnv {
                    cfg: &self.cfg,
                    store: &mut self.store,
                    skills: &self.skills,
                };
                let result = tools::execute(&mut env, &name, &args);
                let is_err = result.starts_with("[Tool error]") || result.starts_with("[Unknown tool]");
                if is_err {
                    consecutive_errors += 1;
                    last_err = snippet(&result, 160);
                    log(&format!("✗ {}", snippet(&result, 200)));
                } else {
                    consecutive_errors = 0;
                    done.push(format!("- {}: {}", name, snippet(&result.replace('\n', " "), 400)));
                    log(&format!("✓ {}", snippet(&result.replace('\n', " "), 400)));
                }
                self.store.append_message(
                    self.session_id,
                    "tool",
                    &result,
                    Some(&name),
                    None,
                    tool_call_id.as_deref(),
                )?;
            }

            if consecutive_errors >= self.cfg.max_tool_errors {
                let msg = format!(
                    "{} consecutive tool calls failed or were flagged as loops; stopping proactively. Last error: {}\nCheck the command syntax and paths, or ask the user to confirm the environment.",
                    consecutive_errors, last_err
                );
                self.store.append_message(
                    self.session_id,
                    "assistant",
                    &msg,
                    None,
                    None,
                    None,
                )?;
                return Ok(TurnReport {
                    text: msg,
                    rounds,
                    tool_calls: total_calls,
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    tokens_per_sec: last_rate,
                    prompt_tokens: last_prompt_tokens,
                    compressed: compressed_any,
                    ctx_chars,
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
        let mut msgs: Vec<(i64, Message)> = stored.into_iter().map(|m| (m.id, from_stored(m))).collect();
        let mut compressed = false;

        let pinned = self.store.pinned_notes(12)?;
        let metas = self.skills.discover();
        // Keep the DB index and the message count in sync
        for m in &metas {
            let _ = self.store.upsert_skill(&m.name, &m.description, &m.path.to_string_lossy());
        }
        let skills_index = self.skills.render_index(&metas);
        let agent_md = prompt::load_agent_md(&self.cfg)?;

        loop {
            let sys_len = char_len(&agent_md)
                + char_len(&skills_index)
                + summary_text.chars().count()
                + pinned.iter().map(|n| char_len(&n.content) + 12).sum::<usize>()
                + 1200;
            let msgs_len: usize = msgs.iter().map(|(_, m)| m.weight()).sum();
            if sys_len + msgs_len <= self.cfg.budget_chars || msgs.len() <= 3 {
                let full = prompt::build(
                    &self.cfg,
                    &agent_md,
                    &skills_index,
                    &pinned,
                    if summary_text.is_empty() { None } else { Some(&summary_text) },
                    self.session_id,
                    sys_len + msgs_len,
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
                Err(e) => format!(
                    "(auto-compaction: summary generation failed: {}) earlier messages: {}; last content: {}",
                    e,
                    chunk.len(),
                    snippet(&chunk.last().map(|(_, m)| m.content.clone()).unwrap_or_default(), 300)
                ),
            };
            self.store.add_summary(self.session_id, upto_id, &new_summary)?;
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
                    if m.tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false) {
                        let names: Vec<String> = m
                            .tool_calls
                            .as_ref()
                            .unwrap()
                            .iter()
                            .map(|c| format!("{}({})", c.function.name, snippet(&c.function.arguments.to_string(), 80)))
                            .collect();
                        transcript.push_str(&format!("Assistant (tool calls): {}\n", names.join("; ")));
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
            Message::user(format!("Summarize the following conversation:\n\n{}", transcript)),
        ];
        let resp: ChatResponse = self.client.chat(&messages, None)?;
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
        self.store.add_summary(self.session_id, upto_id, &new_summary)?;
        Ok(true)
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
