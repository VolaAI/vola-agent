//! System prompt assembly: SYSTEM.md + runtime facts + skill/script indexes + pinned notes + history summary.

use anyhow::{Context, Result};
use std::fs;

use crate::config::Config;
use crate::memory::NoteRow;
use crate::util::{fmt_time, now_secs};

/// Compressor prompt shipped with the binary. Used when agent/MEMORY.md is missing or blank,
/// so the agent keeps working in a fresh checkout without a prompt file.
const DEFAULT_SUMMARIZER: &str = "You are a conversation compressor. Condense the supplied history into at most 700 words, in the language the conversation used.\
Preserve: the user's goals and constraints; confirmed facts (paths, versions, numbers, environment); key commands and their results; unfinished items and the next step.\
No pleasantries, no invention. Mark uncertain items \"unverified\". Output only the summary body.";

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
    scripts_index: &str,
    pinned: &[NoteRow],
    summary: Option<&str>,
    session_id: i64,
) -> String {
    let mut s = String::new();
    s.push_str(agent_md.trim());
    s.push_str("\n\n---\n\n## Runtime environment\n");
    s.push_str(&format!(
        "- Time: {} (UTC{})\n",
        fmt_time(now_secs()),
        offset_str()
    ));
    s.push_str(&format!("- Workdir: {}\n", cfg.home.display()));
    s.push_str(&format!(
        "- Platform: {}/{}, shell={}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        match cfg.shell {
            crate::config::ShellKind::Bash => "bash (git-bash)",
            crate::config::ShellKind::Cmd => "cmd.exe",
            crate::config::ShellKind::PowerShell => "powershell",
        }
    ));
    s.push_str(&format!(
        "- Model: {} (num_ctx={}, temp={})\n",
        cfg.model, cfg.num_ctx, cfg.temperature
    ));
    s.push_str(&format!(
        "- Context budget: ~{} chars; auto-compacts beyond it\n",
        cfg.budget_chars
    ));
    s.push_str(&format!(
        "- Session: #{}, DB: {}\n",
        session_id,
        cfg.db_path.display()
    ));
    s.push_str(&format!(
        "- Skills: {} (skill_* tools)\n",
        cfg.skills_dir.display()
    ));
    s.push_str(&format!(
        "- Scripts: {} (script_* tools)\n",
        cfg.scripts_dir.display()
    ));
    s.push_str(&format!("- Assets: {}\n", cfg.assets_dir.display()));
    s.push_str(&format!(
        "- Limits: cmd timeout {}s (max 600), output cap {} chars\n",
        cfg.cmd_timeout_secs, cfg.output_limit
    ));

    s.push_str("\n## Skills\n");
    s.push_str(skills_index);
    s.push_str("\n\n(Use skill_view before a related task; skill_save after a reusable one.)\n");

    s.push_str("\n## Scripts\n");
    s.push_str(scripts_index);
    s.push_str("\n\n(Use script_run to run one; script_save after a reusable one.)\n");

    if !pinned.is_empty() {
        s.push_str("\n## Pinned memory\n");
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
        s.push_str("(Use memory_search for the rest.)\n");
    }

    if let Some(sum) = summary {
        if !sum.trim().is_empty() {
            s.push_str("\n## Earlier conversation summary\n");
            s.push_str(sum.trim());
            s.push('\n');
        }
    }

    s
}

fn offset_str() -> String {
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
