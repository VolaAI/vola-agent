//! Tool registry: JSON Schema exposed to the model, plus local dispatch and execution.

pub mod fsops;
pub mod shell;

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::Config;
use crate::memory::{MemoryStore, render_message};
use crate::skills::Skills;
use crate::util::truncate_middle;

pub struct ToolEnv<'a> {
    pub cfg: &'a Config,
    pub store: &'a mut MemoryStore,
    pub skills: &'a Skills,
}

/// Tool list exposed to the model (OpenAI function schema).
pub fn schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "run_cmd",
                "description": "Run a shell command in the working directory and return stdout/stderr/exit code. bash by default (git-bash). The command must not be interactive (no prompts for input), and do not truncate output with tail/head. Default timeout is 60 seconds; on timeout the whole process tree is killed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "cmd": {"type": "string", "description": "the command to run, e.g. `ls -la`, `git status`, `cargo build`"},
                        "timeout_secs": {"type": "integer", "description": "timeout in seconds, 1-600, default 60"},
                        "cwd": {"type": "string", "description": "optional working directory (absolute, or relative to the project dir)"},
                        "shell": {"type": "string", "enum": ["bash", "cmd", "powershell"], "description": "optional, switch interpreter"}
                    },
                    "required": ["cmd"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file. Page through large files with offset/limit (line numbers start at 1).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "file path, supports ~ expansion"},
                        "offset": {"type": "integer", "description": "starting line number (optional)"},
                        "limit": {"type": "integer", "description": "number of lines to read, default 2000 (optional)"}
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write a whole file (overwrites), creating directories as needed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string", "description": "full file content"}
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List a directory (directory names end with /, files show their byte size).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "directory path, defaults to the current directory"},
                        "depth": {"type": "integer", "description": "recursion depth 1-4, default 1"}
                    },
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "skill_list",
                "description": "List all the skills you have (name/description/recent use count).",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "skill_view",
                "description": "Read the full steps of one skill. For a related task, check the skills first; do not guess from memory.",
                "parameters": {
                    "type": "object",
                    "properties": {"name": {"type": "string", "description": "skill name (slug)"}},
                    "required": ["name"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "skill_save",
                "description": "Capture a reusable approach as a skill (Markdown). A skill with the same name is overwritten. After finishing a task, capture it proactively: commands, paths, pitfalls, how to verify.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "skill name (lowercase hyphenated, e.g. rust-build-msvc)"},
                        "description": {"type": "string", "description": "one sentence on when to use this skill"},
                        "tags": {"type": "string", "description": "comma-separated tags (optional)"},
                        "body": {"type": "string", "description": "skill body: when it applies, steps, commands, cautions"}
                    },
                    "required": ["name", "description", "body"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "memory_add",
                "description": "Write a long-term memory (facts/preferencess/pitfalls kept across sessions). Facts with pinned=true are injected into the system prompt every time, so keep only the most critical ones (e.g. toolchain paths, build commands).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "content": {"type": "string", "description": "the fact to remember"},
                        "tags": {"type": "string", "description": "comma-separated tags (optional)"},
                        "pinned": {"type": "boolean", "description": "pin it (default false)"},
                        "key": {"type": "string", "description": "optional short key, e.g. msvc-path"}
                    },
                    "required": ["content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "memory_search",
                "description": "Search long-term memory by keyword.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "limit": {"type": "integer", "description": "default 5"}
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "memory_delete",
                "description": "Delete a long-term memory by id.",
                "parameters": {
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                }
            }
        }),
    ]
}

pub fn names() -> Vec<&'static str> {
    vec![
        "run_cmd",
        "read_file",
        "write_file",
        "list_dir",
        "skill_list",
        "skill_view",
        "skill_save",
        "memory_add",
        "memory_search",
        "memory_delete",
    ]
}

// ---------- argument helpers (tolerating the wrong names/types models commonly send) ----------

fn arg_str(args: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(v) = args.get(*k) {
            match v {
                Value::String(s) => {
                    if !s.trim().is_empty() {
                        return Some(s.clone());
                    }
                }
                Value::Number(n) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

fn arg_usize(args: &Value, keys: &[&str]) -> Option<usize> {
    for k in keys {
        if let Some(v) = args.get(*k) {
            match v {
                Value::Number(n) => return n.as_u64().map(|x| x as usize),
                Value::String(s) => return s.trim().parse::<usize>().ok(),
                _ => {}
            }
        }
    }
    None
}

fn arg_bool(args: &Value, keys: &[&str]) -> Option<bool> {
    for k in keys {
        if let Some(v) = args.get(*k) {
            match v {
                Value::Bool(b) => return Some(*b),
                Value::String(s) => {
                    let l = s.trim().to_ascii_lowercase();
                    if ["true", "1", "yes"].contains(&l.as_str()) {
                        return Some(true);
                    }
                    if ["false", "0", "no"].contains(&l.as_str()) {
                        return Some(false);
                    }
                }
                Value::Number(n) => return Some(n.as_i64().unwrap_or(0) != 0),
                _ => {}
            }
        }
    }
    None
}

fn require(args: &Value, keys: &[&str], tool: &str) -> Result<String> {
    arg_str(args, keys).ok_or_else(|| {
        anyhow::anyhow!(
            "missing argument `{}` (tool {}). Re-call following the schema.",
            keys[0],
            tool
        )
    })
}

/// Run a tool and return the text shown to the model (errors come back as text too, so the model can self-correct).
pub fn execute(env: &mut ToolEnv, name: &str, args: &Value) -> String {
    match dispatch(env, name, args) {
        Ok(text) => text,
        Err(e) => format!("[Tool error] {}: {:#}", name, e),
    }
}

fn dispatch(env: &mut ToolEnv, name: &str, args: &Value) -> Result<String> {
    match name {
        "run_cmd" => {
            let cmd = require(args, &["cmd", "command", "cmdline", "script"], "run_cmd")?;
            let timeout = arg_usize(args, &["timeout_secs", "timeout", "timeout_seconds"])
                .unwrap_or(env.cfg.cmd_timeout_secs as usize)
                .clamp(1, 600) as u64;
            let cwd = arg_str(args, &["cwd", "workdir", "dir"]);
            let cwd_path: Option<PathBuf> = cwd.as_deref().map(|c| fsops::resolve(env.cfg, c));
            let shell = arg_str(args, &["shell"]);
            let out = shell::run(
                env.cfg,
                &cmd,
                Duration::from_secs(timeout),
                cwd_path.as_deref(),
                shell.as_deref(),
            )?;
            let mut text = out.render(&cmd);
            let (t, cut) = truncate_middle(&text, env.cfg.output_limit);
            if cut {
                text = format!(
                    "{}\n[hint] the output exceeded the cap and was truncated. If you need the full text, redirect the output to a file and read it in pages with read_file.",
                    t
                );
            }
            Ok(text)
        }
        "read_file" => {
            let path = require(args, &["path", "file", "filename", "filepath"], "read_file")?;
            let offset = arg_usize(args, &["offset", "start", "from"]).unwrap_or(0);
            let limit = arg_usize(args, &["limit", "lines", "count"]).unwrap_or(2000);
            fsops::read_file(env.cfg, &path, offset, limit)
        }
        "write_file" => {
            let path = require(args, &["path", "file", "filename", "filepath"], "write_file")?;
            let content = arg_str(args, &["content", "text", "body", "data"])
                .or_else(|| {
                    args.get("content")
                        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                })
                .ok_or_else(|| anyhow::anyhow!("missing argument `content`"))?;
            fsops::write(env.cfg, &path, &content)
        }
        "list_dir" => {
            let path = arg_str(args, &["path", "dir", "directory"]).unwrap_or_default();
            let depth = arg_usize(args, &["depth", "recursive"]).unwrap_or(1);
            fsops::list_dir(env.cfg, &path, depth)
        }
        "skill_list" => {
            let rows = env.store.skills()?;
            let mut s = format!("[skill library] {} skills (directory {})\n", rows.len(), env.skills.dir().display());
            if rows.is_empty() {
                s.push_str("(no skills yet. Use skill_save to capture one.)\n");
            }
            for r in rows {
                s.push_str(&format!(
                    "- {} — {} (used {} times, file {})\n",
                    r.name, r.description, r.use_count, r.path
                ));
            }
            Ok(s)
        }
        "skill_view" => {
            let name = require(args, &["name", "skill", "skill_name"], "skill_view")?;
            match env.skills.load(&name) {
                Some((meta, body)) => {
                    env.store.touch_skill(&meta.name)?;
                    let (body, cut) = truncate_middle(&body, env.cfg.output_limit);
                    Ok(format!(
                        "[skill] {} ({})\n\n{}{}",
                        meta.name,
                        meta.description,
                        body,
                        if cut { "\n[truncated]" } else { "" }
                    ))
                }
                None => {
                    let available: Vec<String> = env.skills.discover().into_iter().map(|m| m.name).collect();
                    Ok(format!(
                        "[no skill `{}`] available: {}",
                        name,
                        if available.is_empty() { "none".to_string() } else { available.join(", ") }
                    ))
                }
            }
        }
        "skill_save" => {
            let name = require(args, &["name", "skill", "title"], "skill_save")?;
            let description = arg_str(args, &["description", "desc", "when"]).unwrap_or_default();
            let tags = arg_str(args, &["tags"]).unwrap_or_default();
            let body = require(args, &["body", "content", "steps", "text"], "skill_save")?;
            let (meta, existed) = env.skills.save(&name, &description, &tags, &body)?;
            env.store.upsert_skill(&meta.name, &meta.description, &meta.path.to_string_lossy())?;
            Ok(format!(
                "[skill {}] {} -> {}\n[notes] {}\n[body {} chars]",
                if existed { "updated" } else { "created" },
                meta.name,
                meta.path.display(),
                meta.description,
                body.chars().count()
            ))
        }
        "memory_add" => {
            let content = require(args, &["content", "text", "note", "value"], "memory_add")?;
            let tags = arg_str(args, &["tags"]).unwrap_or_default();
            let pinned = arg_bool(args, &["pinned", "pin", "important"]).unwrap_or(false);
            let key = arg_str(args, &["key", "name"]);
            let id = env.store.add_note(&content, &tags, pinned, key.as_deref())?;
            Ok(format!(
                "[memory written] id={} pinned={} tags=[{}]\n{}",
                id, pinned, tags, content
            ))
        }
        "memory_search" => {
            let query = require(args, &["query", "q", "keyword", "text"], "memory_search")?;
            let limit = arg_usize(args, &["limit"]).unwrap_or(5) as i64;
            let rows = env.store.search_notes(&query, limit)?;
            if rows.is_empty() {
                return Ok(format!("[memory] nothing found related to `{}`.", query));
            }
            let mut s = format!("[memory search `{}`] {} hits\n", query, rows.len());
            for r in rows {
                s.push_str(&format!(
                    "\n- id={}{} {} [{}] {}\n  {}\n",
                    r.id,
                    r.key
                        .as_deref()
                        .map(|k| format!(" ({})", k))
                        .unwrap_or_default(),
                    if r.pinned { "📌" } else { "" },
                    r.tags,
                    crate::util::fmt_time(r.updated_at),
                    r.content
                ));
            }
            Ok(s)
        }
        "memory_delete" => {
            let id = arg_usize(args, &["id", "note_id"]).ok_or_else(|| anyhow::anyhow!("missing argument id"))? as i64;
            let ok = env.store.delete_note(id)?;
            Ok(if ok {
                format!("[memory deleted] id={}", id)
            } else {
                format!("[memory] no record with id={}", id)
            })
        }
        other => {
            let _ = render_message("", "", None);
            Ok(format!(
                "[Unknown tool] `{}`. Available tools: {}",
                other,
                names().join(", ")
            ))
        }
    }
}
