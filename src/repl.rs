//! Interactive REPL: slash commands plus natural-language chat.

use anyhow::Result;
use std::io::{BufRead, Write};

use crate::agent::Agent;
use crate::memory::{render_message, session_line};
use crate::tools;
use crate::util::{fmt_time, snippet};

pub fn run(mut agent: Agent) -> Result<()> {
    banner(&agent);
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("\nyou › ");
        std::io::stdout().flush().ok();
        let Some(Ok(line)) = lines.next() else {
            println!("\n(input closed, exiting)");
            break;
        };
        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if input.starts_with('/') {
            if !command(&mut agent, &input)? {
                break;
            }
            continue;
        }
        match agent.respond(&input, |s| println!("  {}", s)) {
            Ok(report) => {
                println!("\nassistant › {}\n", report.text);
                let prompt_tokens = report
                    .prompt_tokens
                    .map(|n| format!(" · prompt {} tokens", n))
                    .unwrap_or_default();
                println!(
                    "  ── {} rounds · {} tool calls · {:.1}s · {} · context {} chars{}{}",
                    report.rounds,
                    report.tool_calls,
                    report.elapsed_secs,
                    report
                        .tokens_per_sec
                        .map(|v| format!("{:.1} tok/s", v))
                        .unwrap_or_else(|| "speed unknown".into()),
                    report.ctx_chars,
                    prompt_tokens,
                    if report.compressed { " · history compacted" } else { "" }
                );
            }
            Err(e) => eprintln!("\n[failed] {:#}\n", e),
        }
    }
    Ok(())
}

fn banner(agent: &Agent) {
    let cfg = &agent.cfg;
    let skills = agent.skills.discover().len();
    println!("vola-agent · local Ollama agent (Rust)");
    println!("{}", cfg.summary());
    println!(
        "working dir {} · {} skills · session #{} · prompt {} · compaction prompt {}",
        cfg.home.display(),
        skills,
        agent.session_id,
        cfg.prompt_path.display(),
        cfg.summarizer_path.display()
    );
    println!("Type in natural language to begin; /help lists commands, /exit quits.");
}

fn command(agent: &mut Agent, line: &str) -> Result<bool> {
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };
    match cmd {
        "/help" | "/?" => {
            println!("commands:");
            for (cmd, desc) in [
                ("/help", "help"),
                ("/clear | /cls", "clear the terminal (context is kept)"),
                ("/exit | /quit", "quit"),
                ("/new", "new session"),
                ("/sessions", "list past sessions"),
                ("/resume <id>", "resume a session"),
                ("/skills", "list the skill library"),
                ("/skill <name>", "show the full skill text"),
                ("/model [name]", "show/switch model"),
                ("/models", "list Ollama models"),
                ("/ctx", "context and compaction status"),
                ("/compress", "compact history into a summary now"),
                ("/history [n]", "recent messages (including tool results)"),
                ("/note <text>", "save a pinned long-term memory"),
                ("/notes [keyword]", "search long-term memory"),
                ("/tools", "list the tools"),
                ("/! <command>", "run shell directly (bypassing the model)"),
                ("/db", "database stats"),
            ] {
                println!("  {:<18}{}", cmd, desc);
            }
        }
        "/clear" | "/cls" => {
            // Clear the terminal display only; session history and context stay intact.
            print!("\x1b[2J\x1b[3J\x1b[H");
            std::io::stdout().flush().ok();
        }
        "/exit" | "/quit" | "/q" => {
            println!("Bye. Session #{} saved to {}", agent.session_id, agent.cfg.db_path.display());
            return Ok(false);
        }
        "/new" => {
            let id = agent.store.new_session("", &agent.cfg.model)?;
            agent.session_id = id;
            println!("new session #{}", id);
        }
        "/sessions" => {
            let rows = agent.store.sessions(20)?;
            if rows.is_empty() {
                println!("(no sessions yet)");
            }
            for r in rows {
                println!("{}", session_line(&r));
            }
        }
        "/resume" => {
            let id: i64 = rest
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("usage: /resume <session id>"))?;
            if agent.store.session_title(id).is_none() {
                println!("no session #{}", id);
            } else {
                agent.session_id = id;
                let n = agent.store.message_count(id)?;
                println!("switched to session #{} ({} messages)", id, n);
            }
        }
        "/skills" => {
            let rows = agent.store.skills()?;
            let dir = agent.skills.dir().display().to_string();
            println!("{} skills in the library ({})", rows.len(), dir);
            for r in &rows {
                println!(
                    "  {} — {} (used {} times, updated {})",
                    r.name,
                    r.description,
                    r.use_count,
                    fmt_time(r.updated_at)
                );
            }
            if rows.is_empty() {
                println!("  (empty. Run one task with the model, then use skill_save to capture it.)");
            }
        }
        "/skill" => {
            if rest.is_empty() {
                println!("usage: /skill <name>");
            } else {
                match agent.skills.load(rest) {
                    Some((meta, body)) => println!("# {} ({}) \n\n{}", meta.name, meta.description, body),
                    None => println!("no skill `{}`", rest),
                }
            }
        }
        "/model" => {
            if rest.is_empty() {
                println!("current model: {} (num_ctx={})", agent.cfg.model, agent.cfg.num_ctx);
            } else {
                agent.cfg.model = rest.to_string();
                agent.client.set_model(rest);
                println!("switched to model {}", rest);
            }
        }
        "/models" => match agent.client.list_models() {
            Ok(models) => {
                for m in models {
                    let size = m
                        .size
                        .map(|bytes| format!("{:>6.2} GB", bytes as f64 / 1e9))
                        .unwrap_or_default();
                    println!("  {:<45} {}  {}", m.id, size, m.owned_by);
                }
            }
            Err(e) => println!("failed to fetch models: {}", e),
        },
        "/ctx" => {
            let chars = agent.context_chars()?;
            let total = agent.store.message_count(agent.session_id)?;
            let summaries = agent.store.summary_count(agent.session_id)?;
            let upto = agent
                .store
                .latest_summary(agent.session_id)?
                .map(|s| s.upto_message_id)
                .unwrap_or(0);
            let live = agent.store.messages_after(agent.session_id, upto)?.len();
            println!(
                "session #{}: {} DB messages ({} uncompacted), {} summaries generated\ncontext sent to the model: about {} chars / budget {} chars ({:.0}%)",
                agent.session_id,
                total,
                live,
                summaries,
                chars,
                agent.cfg.budget_chars,
                chars as f64 / agent.cfg.budget_chars as f64 * 100.0
            );
        }
        "/compress" => match agent.force_compress() {
            Ok(true) => println!("compacted: older messages folded into a summary (see /ctx)"),
            Ok(false) => println!("too few messages, nothing to compact"),
            Err(e) => println!("compaction failed: {:#}", e),
        },
        "/history" => {
            let n: usize = rest.parse().unwrap_or(10);
            if let Some(s) = agent.store.latest_summary(agent.session_id)? {
                println!(
                    "\n[summary · covers through #{}]\n{}",
                    s.upto_message_id,
                    snippet(&s.text, 1200)
                );
            }
            let msgs = agent.store.messages_after(agent.session_id, 0)?;
            let skip = msgs.len().saturating_sub(n);
            for m in &msgs[skip..] {
                println!(
                    "\n[#{} {}] {}",
                    m.id,
                    fmt_time(m.created_at),
                    render_message(&m.role, &snippet(&m.content, 400), m.tool_name.as_deref())
                );
            }
            if msgs.is_empty() {
                println!("(this session has no uncompacted messages yet)");
            }
        }
        "/note" => {
            if rest.is_empty() {
                println!("usage: /note <fact to remember>");
            } else {
                let id = agent.store.add_note(rest, "manual", true, None)?;
                println!("pinned memory id={}; it will be injected into the system prompt from now on", id);
            }
        }
        "/notes" => {
            let rows = if rest.is_empty() {
                agent.store.search_notes("", 20)?
            } else {
                agent.store.search_notes(rest, 20)?
            };
            if rows.is_empty() {
                println!("(no matching memories)");
            }
            for r in rows {
                println!(
                    "  id={}{} {} [{}] {}\n    {}",
                    r.id,
                    r.key
                        .as_deref()
                        .map(|k| format!(" ({})", k))
                        .unwrap_or_default(),
                    if r.pinned { "📌" } else { "  " },
                    r.tags,
                    fmt_time(r.updated_at),
                    r.content
                );
            }
        }
        "/tools" => {
            for s in tools::schemas() {
                let f = &s["function"];
                println!(
                    "  {:<15} {}",
                    f["name"].as_str().unwrap_or("?"),
                    f["description"].as_str().unwrap_or("").lines().next().unwrap_or("")
                );
            }
        }
        "/db" => {
            let stats = agent.store.stats()?;
            println!(
                "database {}: {} sessions · {} messages · {} summaries · {} memories · {} skills",
                agent.cfg.db_path.display(),
                stats.sessions,
                stats.messages,
                stats.summaries,
                stats.notes,
                stats.skills
            );
        }
        "/!" => {
            if rest.is_empty() {
                println!("usage: /! <shell command>");
            } else {
                let out = tools::shell::run(
                    &agent.cfg,
                    rest,
                    std::time::Duration::from_secs(agent.cfg.cmd_timeout_secs),
                    None,
                    None,
                )?;
                print!("{}", out.render(rest));
            }
        }
        other => println!("unknown command {} (see /help)", other),
    }
    Ok(true)
}
