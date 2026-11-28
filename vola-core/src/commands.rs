//! Slash commands shared by the TUI and the GUI.
//!
//! The command layer is deliberately thin: it parses one line, calls the same `Agent` methods
//! the tools use, and returns text plus a few flags the front ends act on (`quit`, `clear`,
//! `reloaded`, `session_changed`).

use anyhow::Result;
use std::sync::Arc;

use crate::agent::Agent;
use crate::llm::LlmClient;
use crate::memory::{render_message, session_line, NoteRow};
use crate::tools;
use crate::util::{fmt_time, snippet};

#[derive(Debug, Default, Clone)]
pub struct CommandOutput {
    pub text: String,
    /// The TUI should exit (`/exit`).
    pub quit: bool,
    /// The TUI should clear the terminal (`/clear`).
    pub clear: bool,
    /// The agent was reloaded; the GUI should refresh its snapshot.
    pub reloaded: bool,
    /// The active session changed; the GUI should reload messages.
    pub session_changed: bool,
}

impl CommandOutput {
    fn text(text: impl Into<String>) -> CommandOutput {
        CommandOutput {
            text: text.into(),
            ..Default::default()
        }
    }
}

pub fn is_command(line: &str) -> bool {
    line.trim_start().starts_with('/')
}

pub fn help_text() -> &'static str {
    "commands:
  /help                          this help
  /model [name|list]             show/switch model, or list models
  /session [new|resume <id>|delete <id>]   list/manage sessions
  /skill [view <name>|delete <name>]       list/manage skills
  /script [view <name>|delete <name>]      list/manage scripts
  /memory [add <text> [--pin]|delete <id>|search <q>]  manage long-term memory
  /history [n]                   recent messages
  /reload                        reload prompts, skills, scripts and the LLM client
  /compress                      compact history now
  /ctx                           context and compaction status
  /tools                         list tools
  /db                            database stats
  /! <command>                   run a shell command directly
  /clear                         clear the terminal (TUI)
  /exit                          quit (TUI)"
}

pub fn execute(agent: &mut Agent, line: &str) -> Result<CommandOutput> {
    let line = line.trim();
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };
    match cmd {
        "/help" | "/?" => Ok(CommandOutput::text(help_text())),
        "/model" => model_cmd(agent, rest),
        "/session" | "/sessions" => session_cmd(agent, rest),
        "/skill" | "/skills" => skill_cmd(agent, rest),
        "/script" | "/scripts" => script_cmd(agent, rest),
        "/memory" | "/notes" => memory_cmd(agent, rest),
        "/history" => history_cmd(agent, rest),
        "/reload" => reload_cmd(agent),
        // Backward-compatible aliases used by the TUI.
        "/new" => session_cmd(agent, "new"),
        "/resume" => session_cmd(agent, &format!("resume {}", rest)),
        "/note" => memory_cmd(agent, &format!("add {} --pin", rest)),
        "/models" => model_cmd(agent, "list"),
        "/compress" => compress_cmd(agent),
        "/ctx" => ctx_cmd(agent),
        "/tools" => Ok(CommandOutput::text(tools_text())),
        "/db" => db_cmd(agent),
        "/clear" | "/cls" => Ok(CommandOutput {
            clear: true,
            ..Default::default()
        }),
        "/exit" | "/quit" | "/q" => Ok(CommandOutput {
            text: format!(
                "Bye. Session #{} saved to {}",
                agent.session_id,
                agent.cfg.db_path.display()
            ),
            quit: true,
            ..Default::default()
        }),
        "/!" => shell_cmd(agent, rest),
        other => Ok(CommandOutput::text(format!(
            "unknown command {} (see /help)",
            other
        ))),
    }
}

fn split_sub(rest: &str) -> (&str, &str) {
    match rest.split_once(char::is_whitespace) {
        Some((s, a)) => (s, a.trim()),
        None => (rest, ""),
    }
}

fn model_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    match rest {
        "" => Ok(CommandOutput::text(format!(
            "model {} (num_ctx={})",
            agent.cfg.model, agent.cfg.num_ctx
        ))),
        "list" => {
            let models = agent.client.list_models()?;
            let mut s = String::new();
            for m in models {
                let size = m
                    .size
                    .map(|b| format!(" {:>6.2} GB", b as f64 / 1e9))
                    .unwrap_or_default();
                s.push_str(&format!("{:<45}{}  {}\n", m.id, size, m.owned_by));
            }
            Ok(CommandOutput::text(s))
        }
        name => {
            agent.cfg.model = name.to_string();
            agent.client = Arc::new(LlmClient::new(&agent.cfg));
            Ok(CommandOutput::text(format!("switched to model {}", name)))
        }
    }
}

fn session_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    let (sub, arg) = split_sub(rest);
    match sub {
        "" | "list" => {
            let rows = agent.store.sessions(50)?;
            if rows.is_empty() {
                return Ok(CommandOutput::text("(no sessions yet)"));
            }
            let mut s = String::new();
            for r in rows {
                s.push_str(&session_line(&r));
                s.push('\n');
            }
            Ok(CommandOutput::text(s))
        }
        "new" => {
            let id = agent.store.new_session("", &agent.cfg.model)?;
            agent.session_id = id;
            Ok(CommandOutput {
                text: format!("new session #{}", id),
                session_changed: true,
                ..Default::default()
            })
        }
        "resume" => {
            let id: i64 = arg
                .parse()
                .map_err(|_| anyhow::anyhow!("usage: /session resume <id>"))?;
            if agent.store.session_title(id).is_none() {
                return Ok(CommandOutput::text(format!("no session #{}", id)));
            }
            agent.session_id = id;
            let n = agent.store.message_count(id)?;
            Ok(CommandOutput {
                text: format!("switched to session #{} ({} messages)", id, n),
                session_changed: true,
                ..Default::default()
            })
        }
        "delete" | "del" => {
            let id: i64 = arg
                .parse()
                .map_err(|_| anyhow::anyhow!("usage: /session delete <id>"))?;
            if !agent.store.delete_session(id)? {
                return Ok(CommandOutput::text(format!("no session #{}", id)));
            }
            let mut text = format!("deleted session #{}", id);
            let mut session_changed = false;
            if agent.session_id == id {
                let new_id = agent.store.new_session("", &agent.cfg.model)?;
                agent.session_id = new_id;
                text.push_str(&format!("; switched to new session #{}", new_id));
                session_changed = true;
            }
            Ok(CommandOutput {
                text,
                session_changed,
                ..Default::default()
            })
        }
        other => Ok(CommandOutput::text(format!(
            "usage: /session [new|resume <id>|delete <id>] (unknown: {})",
            other
        ))),
    }
}

fn skill_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    let (sub, arg) = split_sub(rest);
    match sub {
        "" | "list" => {
            let rows = agent.store.skills()?;
            let mut s = format!(
                "{} skills in {} (use /skill view <name>)\n",
                rows.len(),
                agent.skills.dir().display()
            );
            for r in rows {
                s.push_str(&format!(
                    "- {} — {} (used {} times)\n",
                    r.name, r.description, r.use_count
                ));
            }
            Ok(CommandOutput::text(s))
        }
        "view" | "show" => {
            if arg.is_empty() {
                return Ok(CommandOutput::text("usage: /skill view <name>"));
            }
            Ok(CommandOutput::text(skill_text(agent, arg)))
        }
        "delete" | "del" => {
            if arg.is_empty() {
                return Ok(CommandOutput::text("usage: /skill delete <name>"));
            }
            match agent.skills.delete(arg)? {
                Some(meta) => {
                    agent.store.delete_skill(&meta.name)?;
                    Ok(CommandOutput::text(format!(
                        "deleted skill {} ({})",
                        meta.name,
                        meta.path.display()
                    )))
                }
                None => Ok(CommandOutput::text(format!("no skill `{}`", arg))),
            }
        }
        // Backward compatibility: `/skill <name>` shows the skill.
        name => Ok(CommandOutput::text(skill_text(agent, name))),
    }
}

fn skill_text(agent: &Agent, name: &str) -> String {
    match agent.skills.load(name) {
        Some((meta, body)) => format!("# {} ({})\n\n{}", meta.name, meta.description, body),
        None => format!("no skill `{}`", name),
    }
}

fn script_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    let (sub, arg) = split_sub(rest);
    match sub {
        "" | "list" => {
            let metas = agent.scripts.discover();
            let mut s = format!(
                "{} scripts in {}\n",
                metas.len(),
                agent.scripts.dir().display()
            );
            for m in &metas {
                s.push_str(&format!(
                    "- {} ({}) v{} — {} [{}]\n",
                    m.name,
                    if m.lang.is_empty() {
                        "?"
                    } else {
                        m.lang.as_str()
                    },
                    if m.version.is_empty() {
                        "?"
                    } else {
                        m.version.as_str()
                    },
                    m.when,
                    if m.status.is_empty() {
                        "ok"
                    } else {
                        m.status.as_str()
                    }
                ));
                if !m.usage.is_empty() {
                    s.push_str(&format!("  usage: {}\n", m.usage));
                }
            }
            Ok(CommandOutput::text(s))
        }
        "view" | "show" => {
            if arg.is_empty() {
                return Ok(CommandOutput::text("usage: /script view <name>"));
            }
            Ok(CommandOutput::text(script_text(agent, arg)))
        }
        "delete" | "del" => {
            if arg.is_empty() {
                return Ok(CommandOutput::text("usage: /script delete <name>"));
            }
            let ok = agent.scripts.delete(arg)?;
            agent.scripts.write_index()?;
            Ok(CommandOutput::text(if ok {
                format!("deleted script {}", crate::scripts::normalize_name(arg))
            } else {
                format!("no script `{}`", arg)
            }))
        }
        // Backward compatibility: `/script <name>` shows the script.
        name => Ok(CommandOutput::text(script_text(agent, name))),
    }
}

fn script_text(agent: &Agent, name: &str) -> String {
    match agent.scripts.load(name) {
        Some((meta, text)) => format!("[script] {}\n\n{}", meta.name, text),
        None => format!("no script `{}`", name),
    }
}

fn memory_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    let (sub, arg) = split_sub(rest);
    match sub {
        "" | "list" => {
            let rows = agent.store.search_notes(arg, 50)?;
            Ok(CommandOutput::text(notes_text(&rows)))
        }
        "search" => {
            let rows = agent.store.search_notes(arg, 20)?;
            Ok(CommandOutput::text(notes_text(&rows)))
        }
        "add" => {
            let pinned = arg.contains("--pin");
            let text = arg.replace("--pin", "").trim().to_string();
            if text.is_empty() {
                return Ok(CommandOutput::text("usage: /memory add <text> [--pin]"));
            }
            let id = agent.store.add_note(&text, "manual", pinned, None)?;
            Ok(CommandOutput::text(format!(
                "memory id={} pinned={}",
                id, pinned
            )))
        }
        "delete" | "del" => {
            let id: i64 = arg
                .parse()
                .map_err(|_| anyhow::anyhow!("usage: /memory delete <id>"))?;
            let ok = agent.store.delete_note(id)?;
            Ok(CommandOutput::text(if ok {
                format!("deleted memory id={}", id)
            } else {
                format!("no memory id={}", id)
            }))
        }
        // Backward compatibility: `/memory <query>` searches.
        query => {
            let rows = agent.store.search_notes(query, 20)?;
            Ok(CommandOutput::text(notes_text(&rows)))
        }
    }
}

fn notes_text(rows: &[NoteRow]) -> String {
    if rows.is_empty() {
        return "(no matching memories)".to_string();
    }
    let mut s = String::new();
    for r in rows {
        s.push_str(&format!(
            "id={}{} {} [{}] {}\n  {}\n",
            r.id,
            r.key
                .as_deref()
                .map(|k| format!(" ({})", k))
                .unwrap_or_default(),
            if r.pinned { "📌" } else { "  " },
            r.tags,
            fmt_time(r.updated_at),
            r.content
        ));
    }
    s
}

fn history_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    let n: usize = rest.parse().unwrap_or(10);
    let mut s = String::new();
    if let Some(sum) = agent.store.latest_summary(agent.session_id)? {
        s.push_str(&format!(
            "[summary · through #{}]\n{}\n\n",
            sum.upto_message_id,
            snippet(&sum.text, 1200)
        ));
    }
    let msgs = agent.store.messages_after(agent.session_id, 0)?;
    let skip = msgs.len().saturating_sub(n);
    for m in &msgs[skip..] {
        s.push_str(&format!(
            "[#{} {}] {}\n",
            m.id,
            fmt_time(m.created_at),
            render_message(&m.role, &snippet(&m.content, 400), m.tool_name.as_deref())
        ));
    }
    if msgs.is_empty() {
        s.push_str("(this session has no uncompacted messages yet)\n");
    }
    Ok(CommandOutput::text(s))
}

fn reload_cmd(agent: &mut Agent) -> Result<CommandOutput> {
    agent.reload()?;
    Ok(CommandOutput {
        text: "reloaded prompts, skills, scripts and the LLM client".to_string(),
        reloaded: true,
        ..Default::default()
    })
}

fn compress_cmd(agent: &mut Agent) -> Result<CommandOutput> {
    let compacted = agent.force_compress()?;
    Ok(CommandOutput::text(if compacted {
        "compacted: older messages folded into a summary"
    } else {
        "too few messages, nothing to compact"
    }))
}

fn ctx_cmd(agent: &mut Agent) -> Result<CommandOutput> {
    let chars = agent.context_chars()?;
    let total = agent.store.message_count(agent.session_id)?;
    let summaries = agent.store.summary_count(agent.session_id)?;
    let upto = agent
        .store
        .latest_summary(agent.session_id)?
        .map(|s| s.upto_message_id)
        .unwrap_or(0);
    let live = agent.store.messages_after(agent.session_id, upto)?.len();
    let pct = if agent.cfg.budget_chars > 0 {
        chars as f64 / agent.cfg.budget_chars as f64 * 100.0
    } else {
        0.0
    };
    Ok(CommandOutput::text(format!(
        "session #{}: {} DB messages ({} uncompacted), {} summaries\ncontext: ~{} / {} chars ({:.0}%)",
        agent.session_id, total, live, summaries, chars, agent.cfg.budget_chars, pct
    )))
}

fn db_cmd(agent: &mut Agent) -> Result<CommandOutput> {
    let stats = agent.store.stats()?;
    Ok(CommandOutput::text(format!(
        "database {}: {} sessions · {} messages · {} summaries · {} memories · {} skills",
        agent.cfg.db_path.display(),
        stats.sessions,
        stats.messages,
        stats.summaries,
        stats.notes,
        stats.skills
    )))
}

fn tools_text() -> String {
    let mut s = String::new();
    for schema in tools::schemas() {
        let f = &schema["function"];
        s.push_str(&format!(
            "{:<15} {}\n",
            f["name"].as_str().unwrap_or("?"),
            f["description"]
                .as_str()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
        ));
    }
    s
}

fn shell_cmd(agent: &mut Agent, rest: &str) -> Result<CommandOutput> {
    if rest.is_empty() {
        return Ok(CommandOutput::text("usage: /! <shell command>"));
    }
    let out = tools::shell::run(
        &agent.cfg,
        rest,
        std::time::Duration::from_secs(agent.cfg.cmd_timeout_secs),
        None,
        None,
    )?;
    Ok(CommandOutput::text(out.render(rest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_the_core_commands() {
        let help = help_text();
        for cmd in [
            "/help", "/model", "/session", "/skill", "/script", "/memory", "/history", "/reload",
        ] {
            assert!(help.contains(cmd), "missing {} in help", cmd);
        }
    }

    #[test]
    fn recognizes_slash_commands() {
        assert!(is_command("/help"));
        assert!(is_command("  /model list"));
        assert!(!is_command("hello"));
    }
}
