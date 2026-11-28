//! Tool registry: JSON Schema exposed to the model, plus local dispatch and execution.

pub mod fsops;
pub mod shell;

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::Config;
use crate::memory::{render_message, MemoryStore};
use crate::scripts::Scripts;
use crate::skills::Skills;
use crate::util::truncate_middle;

pub struct ToolEnv<'a> {
    pub cfg: &'a Config,
    pub store: &'a mut MemoryStore,
    pub skills: &'a Skills,
    pub scripts: &'a Scripts,
}

/// Tool list exposed to the model (OpenAI function schema).
pub fn schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "run_cmd",
                "description": "Run a shell command in the working directory and return stdout/stderr/exit code. Uses the configured shell shown as `shell=` in the runtime environment (git-bash, cmd.exe or powershell); do not assume bash. The command must not be interactive (no prompts for input), and do not truncate output with tail/head. Default timeout is 60 seconds; on timeout the whole process tree is killed.",
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
                "description": "List all the skills you have (name/description/recent use count). Query this before adding, changing or deleting a skill.",
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
                "description": "Create or update a skill (Markdown). A skill with the same name is overwritten, so skill_view it first when refactoring. Only save reusable, verified knowledge; after finishing a task, capture it proactively: commands, paths, pitfalls, how to verify.",
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
                "name": "skill_delete",
                "description": "Delete a skill that is obsolete, wrong or superseded. Check skill_list first and never delete a skill that is still referenced. The library manager page (SKILL.md) is protected.",
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
                "name": "script_list",
                "description": "List scripts (name, lang, version, when, usage).",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "script_view",
                "description": "Read a script (metadata + code). Check the index before rewriting.",
                "parameters": {
                    "type": "object",
                    "properties": {"name": {"type": "string", "description": "script name or file name, e.g. pdf-split or pdf-split.py"}},
                    "required": ["name"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "script_save",
                "description": "Create/update a script; the metadata header is generated. Contract: --help, data on stdout, meaningful exit codes, no hardcoded paths, dry-run for destructive ops, non-interactive, deps listed. Verify with one real run before last_verified.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "kebab-case, optional extension (pdf-split or pdf-split.py)"},
                        "lang": {"type": "string", "description": "python, bash, js, powershell, ... (optional if name has an extension)"},
                        "when": {"type": "string", "description": "one sentence: when to use it"},
                        "body": {"type": "string", "description": "script source (no metadata header)"},
                        "deps": {"type": "string", "description": "comma-separated deps (optional)"},
                        "usage": {"type": "string", "description": "usage line, e.g. pdf-split.py <in> --pages 1-10 (optional)"},
                        "version": {"type": "string", "description": "version (optional; auto-bumps on update)"},
                        "last_verified": {"type": "string", "description": "YYYY-MM-DD of last real run (optional)"},
                        "status": {"type": "string", "description": "unverified | broken: reason (optional)"},
                        "platform": {"type": "string", "description": "e.g. win32 if only verified there (optional)"}
                    },
                    "required": ["name", "when", "body"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "script_delete",
                "description": "Delete a script by name.",
                "parameters": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "script_run",
                "description": "Run a library script with args; picks the interpreter from the extension. Returns stdout/stderr/exit code.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "script name or file name"},
                        "args": {"type": "string", "description": "arguments (optional)"},
                        "timeout_secs": {"type": "integer", "description": "seconds, 1-600, default 60"},
                        "cwd": {"type": "string", "description": "working directory (optional)"}
                    },
                    "required": ["name"]
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
        "skill_delete",
        "script_list",
        "script_view",
        "script_save",
        "script_delete",
        "script_run",
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

/// Accept an argument list as a string or a JSON array (models send both).
fn arg_argv(args: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(v) = args.get(*k) {
            match v {
                Value::String(s) if !s.trim().is_empty() => return Some(s.clone()),
                Value::Array(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .map(|item| match item {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect();
                    if !parts.is_empty() {
                        return Some(parts.join(" "));
                    }
                }
                Value::Number(n) => return Some(n.to_string()),
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

/// Build the interpreter command for a script, based on its extension.
fn script_command(meta: &crate::scripts::ScriptMeta, extra: &str) -> String {
    let quoted = format!("\"{}\"", meta.path.display());
    let ext = meta
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let base = match ext.as_str() {
        "py" => format!("python {}", quoted),
        "sh" | "bash" => format!("bash {}", quoted),
        "ps1" => format!(
            "powershell -NoLogo -NoProfile -NonInteractive -File {}",
            quoted
        ),
        "bat" | "cmd" => format!("cmd /c {}", quoted),
        "js" => format!("node {}", quoted),
        "ts" => format!("npx --yes tsx {}", quoted),
        "rb" => format!("ruby {}", quoted),
        "lua" => format!("lua {}", quoted),
        "pl" => format!("perl {}", quoted),
        "php" => format!("php {}", quoted),
        "r" => format!("Rscript {}", quoted),
        "go" => format!("go run {}", quoted),
        _ => quoted,
    };
    let extra = extra.trim();
    if extra.is_empty() {
        base
    } else {
        format!("{} {}", base, extra)
    }
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
            let path = require(
                args,
                &["path", "file", "filename", "filepath"],
                "write_file",
            )?;
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
            let mut s = format!(
                "[skill library] {} skills (directory {})\n",
                rows.len(),
                env.skills.dir().display()
            );
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
                    let available: Vec<String> =
                        env.skills.discover().into_iter().map(|m| m.name).collect();
                    Ok(format!(
                        "[no skill `{}`] available: {}",
                        name,
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
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
            env.store
                .upsert_skill(&meta.name, &meta.description, &meta.path.to_string_lossy())?;
            Ok(format!(
                "[skill {}] {} -> {}\n[notes] {}\n[body {} chars]",
                if existed { "updated" } else { "created" },
                meta.name,
                meta.path.display(),
                meta.description,
                body.chars().count()
            ))
        }
        "skill_delete" => {
            let name = require(args, &["name", "skill", "skill_name"], "skill_delete")?;
            match env.skills.delete(&name)? {
                Some(meta) => {
                    env.store.delete_skill(&meta.name)?;
                    Ok(format!(
                        "[skill deleted] {} ({})\n[note] the file was removed and the index row dropped; update any skill that referenced it.",
                        meta.name,
                        meta.path.display()
                    ))
                }
                None => {
                    let available: Vec<String> =
                        env.skills.discover().into_iter().map(|m| m.name).collect();
                    Ok(format!(
                        "[no skill `{}`] available: {}",
                        name,
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    ))
                }
            }
        }
        "script_list" => {
            let metas = env.scripts.discover();
            let mut s = format!(
                "[scripts] {} in {}\n",
                metas.len(),
                env.scripts.dir().display()
            );
            if metas.is_empty() {
                s.push_str("(none; use script_save after a reusable task)\n");
            }
            for m in &metas {
                let mut line = format!("- {}", m.name);
                let mut tags = Vec::new();
                if !m.lang.is_empty() {
                    tags.push(m.lang.clone());
                }
                if !m.version.is_empty() {
                    tags.push(format!("v{}", m.version));
                }
                if !tags.is_empty() {
                    line.push_str(&format!(" ({})", tags.join(" ")));
                }
                if !m.when.is_empty() {
                    line.push_str(&format!(" — {}", m.when));
                }
                if !m.status.is_empty() {
                    line.push_str(&format!(" [{}]", m.status));
                }
                if !m.usage.is_empty() {
                    line.push_str(&format!("; usage: {}", m.usage));
                }
                line.push_str(&format!("; file: {}", m.path.display()));
                s.push_str(&line);
                s.push('\n');
            }
            Ok(s)
        }
        "script_view" => {
            let name = require(args, &["name", "script", "script_name"], "script_view")?;
            match env.scripts.load(&name) {
                Some((meta, text)) => {
                    let (text, cut) = truncate_middle(&text, env.cfg.output_limit);
                    Ok(format!(
                        "[script] {}\n\n{}{}",
                        meta.name,
                        text,
                        if cut { "\n[truncated]" } else { "" }
                    ))
                }
                None => {
                    let available: Vec<String> =
                        env.scripts.discover().into_iter().map(|m| m.name).collect();
                    Ok(format!(
                        "[no script `{}`] available: {}",
                        name,
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    ))
                }
            }
        }
        "script_save" => {
            let name = require(args, &["name", "script", "title"], "script_save")?;
            let when =
                arg_str(args, &["when", "description", "desc", "trigger"]).unwrap_or_default();
            let body = require(
                args,
                &["body", "content", "code", "source", "text"],
                "script_save",
            )?;
            let lang = arg_str(args, &["lang", "language"]).unwrap_or_default();
            let deps = arg_str(args, &["deps", "dependencies"]).unwrap_or_default();
            let usage = arg_str(args, &["usage", "usage_line"]).unwrap_or_default();
            let version = arg_str(args, &["version"]).unwrap_or_default();
            let last_verified = arg_str(args, &["last_verified", "verified"]).unwrap_or_default();
            let status = arg_str(args, &["status"]).unwrap_or_default();
            let platform = arg_str(args, &["platform"]).unwrap_or_default();
            let spec = crate::scripts::ScriptSpec {
                name: &name,
                lang: &lang,
                when: &when,
                deps: &deps,
                usage: &usage,
                version: &version,
                last_verified: &last_verified,
                status: &status,
                platform: &platform,
                body: &body,
            };
            let (meta, existed) = env.scripts.save(&spec)?;
            env.scripts.write_index()?;
            Ok(format!(
                "[script {}] {} ({} v{}) -> {}",
                if existed { "updated" } else { "created" },
                meta.name,
                if meta.lang.is_empty() {
                    "?"
                } else {
                    meta.lang.as_str()
                },
                if meta.version.is_empty() {
                    "?"
                } else {
                    meta.version.as_str()
                },
                meta.path.display()
            ))
        }
        "script_delete" => {
            let name = require(args, &["name", "script"], "script_delete")?;
            let ok = env.scripts.delete(&name)?;
            env.scripts.write_index()?;
            Ok(if ok {
                format!("[script deleted] {}", crate::scripts::normalize_name(&name))
            } else {
                format!("[script] no script named `{}`", name)
            })
        }
        "script_run" => {
            let name = require(args, &["name", "script", "script_name"], "script_run")?;
            let extra = arg_argv(args, &["args", "arguments", "argv"]).unwrap_or_default();
            let timeout = arg_usize(args, &["timeout_secs", "timeout", "timeout_seconds"])
                .unwrap_or(env.cfg.cmd_timeout_secs as usize)
                .clamp(1, 600) as u64;
            let cwd = arg_str(args, &["cwd", "workdir", "dir"]);
            let cwd_path = cwd
                .as_deref()
                .map(|c| fsops::resolve(env.cfg, c))
                .or_else(|| Some(env.cfg.home.clone()));
            let meta = env.scripts.find(&name).ok_or_else(|| {
                anyhow::anyhow!(
                    "no script named `{}`; use script_list to see the library",
                    name
                )
            })?;
            let cmd = script_command(&meta, &extra);
            let out = shell::run(
                env.cfg,
                &cmd,
                Duration::from_secs(timeout),
                cwd_path.as_deref(),
                None,
            )?;
            let mut text = out.render(&cmd);
            let (t, cut) = truncate_middle(&text, env.cfg.output_limit);
            if cut {
                text = format!(
                    "{}\n[hint] the output exceeded the cap and was truncated. Redirect it to a file and read it in pages with read_file.",
                    t
                );
            }
            Ok(text)
        }
        "memory_add" => {
            let content = require(args, &["content", "text", "note", "value"], "memory_add")?;
            let tags = arg_str(args, &["tags"]).unwrap_or_default();
            let pinned = arg_bool(args, &["pinned", "pin", "important"]).unwrap_or(false);
            let key = arg_str(args, &["key", "name"]);
            let id = env
                .store
                .add_note(&content, &tags, pinned, key.as_deref())?;
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
            let id = arg_usize(args, &["id", "note_id"])
                .ok_or_else(|| anyhow::anyhow!("missing argument id"))? as i64;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::ScriptMeta;
    use std::path::PathBuf;

    fn meta(path: &str) -> ScriptMeta {
        ScriptMeta {
            name: "demo".to_string(),
            when: String::new(),
            lang: String::new(),
            deps: String::new(),
            usage: String::new(),
            version: String::new(),
            last_verified: String::new(),
            status: String::new(),
            platform: String::new(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn script_command_picks_the_interpreter() {
        assert!(script_command(&meta("pdf-split.py"), "").starts_with("python "));
        assert!(script_command(&meta("port-check.sh"), "").starts_with("bash "));
        assert!(script_command(&meta("run.ps1"), "").contains("-File "));
        assert!(script_command(&meta("run.bat"), "").starts_with("cmd /c "));
        assert!(script_command(&meta("app.js"), "").starts_with("node "));
        assert_eq!(
            script_command(&meta("pdf-split.py"), "--pages 1-10"),
            "python \"pdf-split.py\" --pages 1-10"
        );
    }
}
