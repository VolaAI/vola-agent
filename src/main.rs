//! vola-agent: a command-line client for a local Ollama agent.

mod agent;
mod config;
mod console;
mod interrupt;
mod llm;
mod memory;
mod prompt;
mod repl;
mod skills;
mod tools;
mod util;

use anyhow::{bail, Context, Result};
use config::Config;
use std::path::{Path, PathBuf};

fn main() {
    console::init();
    let result = run();
    console::restore();
    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options::default();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let a = raw[i].clone();
        if !a.starts_with('-') {
            positional.push(a);
            i += 1;
            continue;
        }
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        match key.as_str() {
            "--model" | "-m" => opts.model = Some(value_of(&raw, &mut i, inline, &key)?),
            "--url" => opts.url = Some(value_of(&raw, &mut i, inline, &key)?),
            "--db" => opts.db = Some(value_of(&raw, &mut i, inline, &key)?),
            "--prompt" => opts.prompt = Some(value_of(&raw, &mut i, inline, &key)?),
            "--summarizer" => opts.summarizer = Some(value_of(&raw, &mut i, inline, &key)?),
            "--home" => opts.home = Some(value_of(&raw, &mut i, inline, &key)?),
            "--skills" => opts.skills = Some(value_of(&raw, &mut i, inline, &key)?),
            "--config" | "-c" => opts.config = Some(value_of(&raw, &mut i, inline, &key)?),
            "--assets" => opts.assets = Some(value_of(&raw, &mut i, inline, &key)?),
            "--scripts" => opts.scripts = Some(value_of(&raw, &mut i, inline, &key)?),
            "--session" | "-s" => {
                let v = value_of(&raw, &mut i, inline, &key)?;
                opts.session = v.parse().ok();
            }
            "--num-ctx" => {
                let v = value_of(&raw, &mut i, inline, &key)?;
                opts.num_ctx = v.parse().ok();
            }
            "--verbose" | "-v" => opts.verbose = true,
            "--pin" => positional.push(a.clone()),
            "--allow-dangerous" => opts.allow_dangerous = true,
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--version" | "-V" => {
                println!("vola-agent {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => bail!("unknown option {} (see -h for usage)", other),
        }
        i += 1;
    }

    // Subcommand: the first positional argument
    let sub = positional.first().cloned().unwrap_or_default();
    let args: Vec<String> = positional.iter().skip(1).cloned().collect();

    let mut cfg = match opts.config.as_deref() {
        Some(p) => Config::load_from(Some(Path::new(p)))?,
        None => Config::load()?,
    };
    if let Some(v) = opts.model {
        cfg.model = v;
    }
    if let Some(v) = &opts.url {
        cfg.base_url = v.trim_end_matches('/').to_string();
    }
    if let Some(v) = &opts.db {
        cfg.db_path = PathBuf::from(v);
    }
    if let Some(v) = &opts.prompt {
        cfg.prompt_path = PathBuf::from(v);
    }
    if let Some(v) = &opts.summarizer {
        cfg.summarizer_path = PathBuf::from(v);
    }
    if let Some(v) = &opts.skills {
        cfg.skills_dir = PathBuf::from(v);
    }
    if let Some(v) = &opts.assets {
        cfg.assets_dir = PathBuf::from(v);
    }
    if let Some(v) = &opts.scripts {
        cfg.scripts_dir = PathBuf::from(v);
    }
    if let Some(v) = &opts.home {
        cfg.home = PathBuf::from(v);
        if opts.prompt.is_none() {
            cfg.prompt_path = Config::default_prompt_path(&cfg.home);
        }
        if opts.summarizer.is_none() {
            cfg.summarizer_path = Config::default_summarizer_path(&cfg.home);
        }
        if opts.db.is_none() {
            cfg.db_path = Config::default_db_path(&cfg.home);
        }
        if opts.skills.is_none() {
            cfg.skills_dir = Config::default_skills_dir(&cfg.home);
        }
        if opts.assets.is_none() {
            cfg.assets_dir = Config::default_assets_dir(&cfg.home);
        }
        if opts.scripts.is_none() {
            cfg.scripts_dir = Config::default_scripts_dir(&cfg.home);
        }
    }
    if let Some(v) = opts.num_ctx {
        cfg.num_ctx = v;
        cfg.budget_chars = (v as usize) * 2;
    }
    if opts.verbose {
        cfg.verbose = true;
    }
    if opts.allow_dangerous {
        cfg.allow_dangerous = true;
    }

    match sub.as_str() {
        "" | "chat" => {
            let agent = agent::Agent::new(cfg, opts.session)?;
            repl::run(agent)
        }
        "ask" => {
            let text = args.join(" ");
            if text.trim().is_empty() {
                bail!("usage: vola-agent ask \"your question\"");
            }
            let mut agent = agent::Agent::new(cfg, opts.session)?;
            let report = agent.respond(&text, |s| eprintln!("{}", s))?;
            println!("{}", report.text);
            let prompt_tokens = report
                .prompt_tokens
                .map(|n| format!(" · prompt {} tokens", n))
                .unwrap_or_default();
            eprintln!(
                "\n[{} rounds · {} tool calls · {:.1}s · {}{}{}]",
                report.rounds,
                report.tool_calls,
                report.elapsed_secs,
                report
                    .tokens_per_sec
                    .map(|v| format!("{:.1} tok/s", v))
                    .unwrap_or_else(|| "speed unknown".into()),
                prompt_tokens,
                if report.compressed {
                    " · history compacted"
                } else {
                    ""
                }
            );
            Ok(())
        }
        "skills" => {
            let skills = skills::Skills::new(cfg.skills_dir.clone());
            let metas = skills.discover();
            if metas.is_empty() {
                println!("(skill directory is empty: {})", cfg.skills_dir.display());
            }
            for m in metas {
                let tags = if m.tags.is_empty() {
                    String::new()
                } else {
                    format!("[{}] ", m.tags)
                };
                println!(
                    "{:<28} {}\n    {}{}",
                    m.name,
                    m.description,
                    tags,
                    m.path.display()
                );
            }
            Ok(())
        }
        "memory" => memory_cmd(&cfg, &args),
        "models" => {
            let client = llm::LlmClient::new(&cfg);
            let models = client.list_models().with_context(|| {
                format!(
                    "failed to connect to provider `{}` ({}) at {}; for Ollama start G:\\Ollama\\start-ollama.bat",
                    cfg.provider_tag, cfg.provider_type, cfg.base_url
                )
            })?;
            for m in models {
                let size = m
                    .size
                    .map(|bytes| format!("{:>6.2} GB", bytes as f64 / 1e9))
                    .unwrap_or_default();
                println!("{:<45} {}  {}", m.id, size, m.owned_by);
            }
            Ok(())
        }
        "sessions" => {
            let store = memory::MemoryStore::open(&cfg.db_path)?;
            for r in store.sessions(50)? {
                println!("{}", memory::session_line(&r));
            }
            Ok(())
        }
        "tools" => {
            for s in tools::schemas() {
                let f = &s["function"];
                println!(
                    "{}\n  {}",
                    f["name"].as_str().unwrap_or("?"),
                    f["description"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        "init" => {
            init_project(&cfg)?;
            Ok(())
        }
        other => {
            print_help();
            bail!("unknown subcommand: {}", other)
        }
    }
}

#[derive(Default)]
struct Options {
    model: Option<String>,
    url: Option<String>,
    db: Option<String>,
    prompt: Option<String>,
    summarizer: Option<String>,
    home: Option<String>,
    config: Option<String>,
    assets: Option<String>,
    scripts: Option<String>,
    skills: Option<String>,
    session: Option<i64>,
    num_ctx: Option<u32>,
    verbose: bool,
    allow_dangerous: bool,
}

/// Read an option value: accepts `--key=value`, otherwise consumes the next argument.
fn value_of(raw: &[String], i: &mut usize, inline: Option<String>, key: &str) -> Result<String> {
    if let Some(v) = inline {
        return Ok(v);
    }
    *i += 1;
    raw.get(*i)
        .cloned()
        .with_context(|| format!("option {} is missing a value", key))
}

fn memory_cmd(cfg: &Config, args: &[String]) -> Result<()> {
    let mut store = memory::MemoryStore::open(&cfg.db_path)?;
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "add" => {
            let content = args[1..].join(" ");
            if content.trim().is_empty() {
                bail!("usage: vola-agent memory add <content> [--pin]");
            }
            let pinned = args.iter().any(|a| a == "--pin");
            let text = content.replace("--pin", "").trim().to_string();
            let id = store.add_note(&text, "cli", pinned, None)?;
            println!("wrote id={} pinned={}", id, pinned);
        }
        "search" => {
            let q = args[1..].join(" ");
            for r in store.search_notes(&q, 20)? {
                println!(
                    "id={}{} [{}] {}\n  {}",
                    r.id,
                    r.key
                        .as_deref()
                        .map(|k| format!(" ({})", k))
                        .unwrap_or_default(),
                    r.tags,
                    util::fmt_time(r.updated_at),
                    r.content
                );
            }
        }
        "delete" | "del" => {
            let id: i64 = args
                .get(1)
                .and_then(|v| v.parse().ok())
                .context("usage: memory delete <id>")?;
            println!(
                "{}",
                if store.delete_note(id)? {
                    "deleted"
                } else {
                    "not found"
                }
            );
        }
        "list" | _ => {
            let q = args.get(1).map(|s| s.as_str()).unwrap_or("");
            for r in store.search_notes(q, 50)? {
                println!(
                    "id={}{} {} [{}] {}",
                    r.id,
                    r.key
                        .as_deref()
                        .map(|k| format!(" ({})", k))
                        .unwrap_or_default(),
                    if r.pinned { "📌" } else { "  " },
                    r.tags,
                    r.content
                );
            }
        }
    }
    Ok(())
}

fn init_project(cfg: &Config) -> Result<()> {
    let references = cfg.skills_dir.join("references");
    for dir in [
        &cfg.assets_dir,
        &cfg.scripts_dir,
        &cfg.skills_dir,
        &references,
    ] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create the directory: {}", dir.display()))?;
    }
    for (path, sample, label) in [
        (
            cfg.home.join("config.json"),
            include_str!("../config.json"),
            "config file",
        ),
        (
            cfg.prompt_path.clone(),
            include_str!("../agent/SYSTEM.md"),
            "system prompt",
        ),
        (
            cfg.summarizer_path.clone(),
            include_str!("../agent/MEMORY.md"),
            "compaction prompt",
        ),
        (
            cfg.skills_dir.join("SKILL.md"),
            include_str!("../agent/skills/SKILL.md"),
            "skill library page",
        ),
    ] {
        if path.exists() {
            println!("{} already exists at {}", label, path.display());
        } else {
            std::fs::write(&path, sample)
                .with_context(|| format!("write failed: {}", path.display()))?;
            println!("created {} {}", label, path.display());
        }
    }
    println!("assets directory {}", cfg.assets_dir.display());
    println!("scripts directory {}", cfg.scripts_dir.display());
    println!("skill directory {}", cfg.skills_dir.display());
    println!("database {}", cfg.db_path.display());
    Ok(())
}

fn print_help() {
    println!(
        "vola-agent {} \u{2014} local Ollama agent (Rust)",
        env!("CARGO_PKG_VERSION")
    );
    println!("\nusage:");
    for (cmd, desc) in [
        (
            "vola-agent",
            "enter interactive mode (default command chat)",
        ),
        ("vola-agent ask \"<question>\"", "one-shot question"),
        ("vola-agent skills", "list skills"),
        (
            "vola-agent memory add <content> [--pin] | search <keyword> | delete <id> | list",
            "",
        ),
        (
            "vola-agent models",
            "list models from the selected provider",
        ),
        ("vola-agent sessions", "list past sessions"),
        ("vola-agent tools", "list tools"),
        (
            "vola-agent init",
            "create config.json, asset dirs and prompt skeletons",
        ),
    ] {
        println!("{}", format!("  {:<34}{}", cmd, desc).trim_end());
    }
    println!("\noptions:");
    for (a, b, c) in [
        ("-m, --model <name>", "--url <Ollama URL>", "--db <path>"),
        ("--config <json>", "--assets <dir>", "--scripts <dir>"),
        ("--prompt <SYSTEM.md>", "--summarizer <MEMORY.md>", ""),
        ("--skills <dir>", "--home <project dir>", ""),
        ("-s, --session <id>", "--num-ctx <n>", "-v, --verbose"),
        ("--allow-dangerous", "--version", "-h, --help"),
    ] {
        println!("{}", format!("  {:<22}{:<26}{}", a, b, c).trim_end());
    }
    println!("\nenvironment:");
    for chunk in [
        "VOLA_AGENT_CONFIG / VOLA_AGENT_URL / VOLA_AGENT_MODEL / VOLA_AGENT_DB / VOLA_AGENT_PROMPT /",
        "VOLA_AGENT_SUMMARIZER / VOLA_AGENT_ASSETS / VOLA_AGENT_SCRIPTS / VOLA_AGENT_SKILLS /",
        "VOLA_AGENT_HOME / VOLA_AGENT_NUM_CTX / VOLA_AGENT_TEMPERATURE / VOLA_AGENT_API_KEY /",
        "OPENAI_API_KEY / DEEPSEEK_API_KEY / VOLA_AGENT_BUDGET / VOLA_AGENT_CMD_TIMEOUT /",
        "VOLA_AGENT_OUTPUT_LIMIT / VOLA_AGENT_MAX_ROUNDS / VOLA_AGENT_MAX_TOOL_ERRORS /",
        "VOLA_AGENT_REQUEST_TIMEOUT / VOLA_AGENT_VERBOSE / VOLA_AGENT_ALLOW_DANGEROUS /",
        "VOLA_AGENT_SHELL / VOLA_AGENT_BASH",
    ] {
        println!("  {}", chunk);
    }
}
