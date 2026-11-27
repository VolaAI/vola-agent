//! System prompt assembly: agent/SYSTEM.md body + environment facts + skill index + pinned notes + history summary.

use anyhow::{Context, Result};
use std::fs;

use crate::config::Config;
use crate::memory::NoteRow;
use crate::util::{fmt_time, now_secs};

/// Compressor prompt shipped with the binary. Used when agent/MEMORY.md is missing or blank,
/// so the agent keeps working in a fresh checkout without a prompt file.
const DEFAULT_SUMMARIZER: &str = "You are a conversation compressor. Condense the given conversation history into a summary of at most 700 words, written in the language the conversation used.\
Keep: the user's goals and constraints; confirmed facts (paths, versions, numbers, environment); the key commands executed and their results; unfinished items and next steps.\
No pleasantries, no invented details; mark anything uncertain as 'unconfirmed'. Output only the summary body.";

pub fn load_agent_md(cfg: &Config) -> Result<String> {
    fs::read_to_string(&cfg.prompt_path).with_context(|| {
        format!(
            "failed to read the prompt file: {} (use --prompt to point at another path)",
            cfg.prompt_path.display()
        )
    })
}

/// Read the external compressor prompt (agent/MEMORY.md), dropping `<!-- ... -->` editor notes
/// so they never reach the model. Falls back to the embedded default on read failure,
/// empty file, or a file that contains nothing but comments.
pub fn load_summarizer(cfg: &Config) -> String {
    pick_summarizer(fs::read_to_string(&cfg.summarizer_path).ok())
}

/// Turn the raw file contents into the prompt actually sent, or the embedded default
/// when the file is unreadable, blank, or holds nothing but editor comments.
fn pick_summarizer(raw: Option<String>) -> String {
    let text = strip_html_comments(&raw.unwrap_or_default());
    if text.trim().is_empty() {
        DEFAULT_SUMMARIZER.to_string()
    } else {
        text.trim().to_string()
    }
}

fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn build(
    cfg: &Config,
    agent_md: &str,
    skills_index: &str,
    pinned: &[NoteRow],
    summary: Option<&str>,
    session_id: i64,
    ctx_chars: usize,
) -> String {
    let mut s = String::new();
    s.push_str(agent_md.trim());
    s.push_str("\n\n---\n\n## Runtime environment\n");
    s.push_str(&format!(
        "- Current time: {} (UTC{})\n",
        fmt_time(now_secs()),
        offset_str(cfg)
    ));
    s.push_str(&format!("- Working directory: {}\n", cfg.home.display()));
    s.push_str(&format!(
        "- Platform: {} / {} / shell={}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        match cfg.shell {
            crate::config::ShellKind::Bash => "bash (git-bash)",
            crate::config::ShellKind::Cmd => "cmd.exe",
            crate::config::ShellKind::PowerShell => "powershell",
        }
    ));
    s.push_str(&format!(
        "- Model: {} (num_ctx={}, temperature={})\n",
        cfg.model, cfg.num_ctx, cfg.temperature
    ));
    s.push_str(&format!(
        "- Context budget: about {} chars (current system prompt {} chars); history is auto-compacted when this is exceeded\n",
        cfg.budget_chars, ctx_chars
    ));
    s.push_str(&format!(
        "- Session: #{}, database: {}\n",
        session_id,
        cfg.db_path.display()
    ));
    s.push_str(&format!(
        "- Skill directory: {} (managed with the skill_* tools)\n",
        cfg.skills_dir.display()
    ));
    s.push_str(&format!(
        "- Assets directory: {} (static resources and reference data)\n",
        cfg.assets_dir.display()
    ));
    s.push_str(&format!(
        "- Scripts directory: {} (reusable automation scripts; run them with run_cmd)\n",
        cfg.scripts_dir.display()
    ));
    s.push_str(&format!(
        "- Command timeout: {}s by default (at most 600s per command); output cap: {} chars\n",
        cfg.cmd_timeout_secs, cfg.output_limit
    ));

    s.push_str("\n## Skills you already have (SKILL index)\n");
    s.push_str(skills_index);
    s.push_str("\n\n(Before a related task, read the steps with skill_view; after finishing, capture any new reusable approach with skill_save.)\n");

    if !pinned.is_empty() {
        s.push_str("\n## Pinned long-term memory (injected every time)\n");
        for n in pinned {
            s.push_str(&format!(
                "- {}{}\n",
                if n.tags.is_empty() {
                    String::new()
                } else {
                    format!("[{}] ", n.tags)
                },
                n.content.trim()
            ));
        }
        s.push_str("(Use memory_search for memories that are not pinned.)\n");
    }

    if let Some(sum) = summary {
        if !sum.trim().is_empty() {
            s.push_str("\n## Compacted summary of earlier conversation (older messages have been folded in)\n");
            s.push_str(sum.trim());
            s.push('\n');
        }
    }

    s
}

fn offset_str(cfg: &Config) -> String {
    let _ = cfg;
    match std::env::var("VOLA_AGENT_UTC_OFFSET")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        Some(v) if v >= 0 => format!("+{}", v),
        Some(v) => v.to_string(),
        None => "+8".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_comments_are_stripped() {
        assert_eq!(strip_html_comments("a<!-- note -->b"), "ab");
        assert_eq!(strip_html_comments("<!-- unclosed comment"), "");
        assert_eq!(
            strip_html_comments(
                "body<!-- multiline
comment -->tail"
            )
            .trim(),
            "bodytail"
        );
    }

    #[test]
    fn summarizer_uses_file_text_and_falls_back() {
        assert_eq!(
            pick_summarizer(Some(
                "# Conversation compressor\nCondense to 700 words.".into()
            )),
            "# Conversation compressor\nCondense to 700 words."
        );
        // File present but only comments, or unreadable: embedded default takes over.
        assert_eq!(
            pick_summarizer(Some("<!-- only a comment -->\n".into())),
            DEFAULT_SUMMARIZER.to_string()
        );
        assert_eq!(pick_summarizer(None), DEFAULT_SUMMARIZER.to_string());
    }
}
