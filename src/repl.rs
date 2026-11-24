//! Interactive REPL: slash commands plus natural-language chat.

use anyhow::Result;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent::{Agent, TurnProgress};
use crate::memory::{render_message, session_line, NoteRow};
use crate::tools;
use crate::util::{fmt_time, snippet};

pub fn run(mut agent: Agent) -> Result<()> {
    if let Err(e) = crate::interrupt::install() {
        eprintln!("[warn] {}", e);
    }
    banner(&agent);
    // Animate only for a real interactive session: piped stdin has no echo that would put
    // the cursor on a fresh line, so a status line would overwrite the prompt.
    let animated = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        crate::interrupt::prompt_mode();
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
        crate::interrupt::begin_turn();
        if input.starts_with('/') {
            let keep = command(&mut agent, &input)?;
            crate::interrupt::end_turn();
            if !keep {
                break;
            }
            continue;
        }

        let output = Arc::new(Mutex::new(()));
        let spinner = Spinner::start(
            agent.cfg.budget_chars,
            agent.cfg.num_ctx,
            animated,
            Arc::clone(&output),
        );
        let progress_status = Arc::clone(&spinner.status);
        let report_status = Arc::clone(&spinner.status);
        let log = {
            let output = Arc::clone(&output);
            move |s: &str| {
                let _guard = output.lock();
                if animated {
                    print!("\x1b[2K\r");
                }
                println!("  {}", s);
                let _ = std::io::stdout().flush();
            }
        };
        let mut on_progress = move |p: &TurnProgress| {
            if let Ok(mut st) = progress_status.lock() {
                st.ctx_chars = p.ctx_chars;
                st.budget_chars = p.budget_chars;
                st.num_ctx = p.num_ctx;
                st.prompt_tokens = p.prompt_tokens;
                st.completion_tokens = p.completion_tokens;
                st.total_completion_tokens = p.total_completion_tokens;
                st.initialized = true;
            }
        };
        let outcome = agent.respond_with(&input, log, &mut on_progress);
        spinner.finish();
        match outcome {
            Ok(report) if report.cancelled => {
                let out = report_status
                    .lock()
                    .map(|st| st.total_completion_tokens)
                    .unwrap_or(0);
                println!(
                    "\n  ── cancelled after {:.1}s · {} rounds · {} tool calls · out {} tok\n",
                    report.elapsed_secs,
                    report.rounds,
                    report.tool_calls,
                    compact(out)
                );
            }
            Ok(report) => {
                println!("\nassistant › {}\n", report.text);
                let tokens = match report.prompt_tokens {
                    Some(prompt) => {
                        let pct = if agent.cfg.num_ctx > 0 {
                            prompt as f64 / agent.cfg.num_ctx as f64 * 100.0
                        } else {
                            0.0
                        };
                        format!(
                            " · prompt {}/{} tok ({:.0}%) · out {} tok",
                            compact(prompt),
                            compact(agent.cfg.num_ctx as u64),
                            pct,
                            compact(report.completion_tokens)
                        )
                    }
                    None => String::new(),
                };
                println!(
                    "  ── {} rounds · {} tool calls · {:.1}s · {} · ctx {} chars{}{}",
                    report.rounds,
                    report.tool_calls,
                    report.elapsed_secs,
                    report
                        .tokens_per_sec
                        .map(|v| format!("{:.1} tok/s", v))
                        .unwrap_or_else(|| "speed unknown".into()),
                    report.ctx_chars,
                    tokens,
                    if report.compressed {
                        " · history compacted"
                    } else {
                        ""
                    }
                );
            }
            Err(e) => eprintln!("\n[failed] {:#}\n", e),
        }
        crate::interrupt::end_turn();
    }
    Ok(())
}

/// Numbers the status line shows, shared between the model turn and the spinner thread.
#[derive(Default)]
struct LiveStatus {
    initialized: bool,
    ctx_chars: usize,
    budget_chars: usize,
    num_ctx: u32,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_completion_tokens: u64,
}

/// Left-margin spinner with live context and token numbers, shown while the agent works.
struct Spinner {
    status: Arc<Mutex<LiveStatus>>,
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(budget_chars: usize, num_ctx: u32, animated: bool, output: Arc<Mutex<()>>) -> Spinner {
        let status = Arc::new(Mutex::new(LiveStatus {
            budget_chars,
            num_ctx,
            ..Default::default()
        }));
        let running = Arc::new(AtomicBool::new(true));
        let handle = if animated {
            let status = Arc::clone(&status);
            let running = Arc::clone(&running);
            Some(std::thread::spawn(move || {
                // ASCII frames render in every Windows console font.
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let started = Instant::now();
                let mut frame = 0usize;
                while running.load(Ordering::SeqCst) {
                    {
                        let _guard = output.lock();
                        if let Ok(st) = status.lock() {
                            if st.initialized {
                                print!(
                                    "\x1b[2K\r{}",
                                    status_line(
                                        &st,
                                        FRAMES[frame],
                                        started.elapsed().as_secs_f64()
                                    )
                                );
                                let _ = std::io::stdout().flush();
                            }
                        }
                    }
                    frame = (frame + 1) % FRAMES.len();
                    std::thread::sleep(Duration::from_millis(100));
                }
                if let Ok(_guard) = output.lock() {
                    print!("\x1b[2K\r");
                    let _ = std::io::stdout().flush();
                }
            }))
        } else {
            None
        };
        Spinner {
            status,
            running,
            handle,
        }
    }

    fn finish(mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn status_line(st: &LiveStatus, frame: &str, elapsed: f64) -> String {
    let ctx_pct = if st.budget_chars > 0 {
        st.ctx_chars as f64 / st.budget_chars as f64 * 100.0
    } else {
        0.0
    };
    let tokens = match st.prompt_tokens {
        Some(prompt) => {
            let tok_pct = if st.num_ctx > 0 {
                prompt as f64 / st.num_ctx as f64 * 100.0
            } else {
                0.0
            };
            match st.completion_tokens {
                Some(completion) => format!(
                    " · tok {}/{} ({:.0}%) · out {}",
                    compact(prompt),
                    compact(st.num_ctx as u64),
                    tok_pct,
                    compact(completion)
                ),
                None => format!(
                    " · tok {}/{} ({:.0}%)",
                    compact(prompt),
                    compact(st.num_ctx as u64),
                    tok_pct
                ),
            }
        }
        None => format!(" · tok est~{}", compact(st.ctx_chars as u64 / 2)),
    };
    format!(
        "  {} thinking {:.1}s · ctx {}/{} ({:.0}%){}",
        frame,
        elapsed,
        compact(st.ctx_chars as u64),
        compact(st.budget_chars as u64),
        ctx_pct,
        tokens
    )
}

fn compact(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn print_notes(rows: &[NoteRow]) {
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
                ("/memory list [kw]", "list long-term memories"),
                ("/memory del <id>", "delete a long-term memory by id"),
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
            println!(
                "Bye. Session #{} saved to {}",
                agent.session_id,
                agent.cfg.db_path.display()
            );
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
                println!(
                    "  (empty. Run one task with the model, then use skill_save to capture it.)"
                );
            }
        }
        "/skill" => {
            if rest.is_empty() {
                println!("usage: /skill <name>");
            } else {
                match agent.skills.load(rest) {
                    Some((meta, body)) => {
                        println!("# {} ({}) \n\n{}", meta.name, meta.description, body)
                    }
                    None => println!("no skill `{}`", rest),
                }
            }
        }
        "/model" => {
            if rest.is_empty() {
                println!(
                    "current model: {} (num_ctx={})",
                    agent.cfg.model, agent.cfg.num_ctx
                );
            } else {
                agent.cfg.model = rest.to_string();
                agent.client = std::sync::Arc::new(crate::llm::LlmClient::new(&agent.cfg));
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
                println!(
                    "pinned memory id={}; it will be injected into the system prompt from now on",
                    id
                );
            }
        }
        "/notes" => {
            let rows = if rest.is_empty() {
                agent.store.search_notes("", 20)?
            } else {
                agent.store.search_notes(rest, 20)?
            };
            print_notes(&rows);
        }
        "/memory" => {
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s, a.trim()),
                None => (rest, ""),
            };
            match sub {
                "" | "list" => {
                    let rows = if arg.is_empty() {
                        agent.store.search_notes("", 50)?
                    } else {
                        agent.store.search_notes(arg, 20)?
                    };
                    print_notes(&rows);
                }
                "del" | "delete" => {
                    let id: i64 = arg
                        .parse()
                        .map_err(|_| anyhow::anyhow!("usage: /memory del <id>"))?;
                    if agent.store.delete_note(id)? {
                        println!("deleted memory id={}", id);
                    } else {
                        println!("no memory with id={}", id);
                    }
                }
                other => println!(
                    "usage: /memory list [keyword] | /memory del <id> (unknown subcommand: {})",
                    other
                ),
            }
        }
        "/tools" => {
            for s in tools::schemas() {
                let f = &s["function"];
                println!(
                    "  {:<15} {}",
                    f["name"].as_str().unwrap_or("?"),
                    f["description"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_reports_context_and_tokens() {
        let status = LiveStatus {
            initialized: true,
            ctx_chars: 6213,
            budget_chars: 32768,
            num_ctx: 16384,
            prompt_tokens: Some(2651),
            completion_tokens: Some(420),
            total_completion_tokens: 420,
        };
        let line = status_line(&status, "|", 3.2);
        assert!(line.contains("ctx 6213/32.8k (19%)"), "{}", line);
        assert!(line.contains("tok 2651/16.4k (16%)"), "{}", line);
        assert!(line.contains("thinking"), "{}", line);
        assert!(line.contains("out 420"), "{}", line);

        let fresh = LiveStatus {
            ctx_chars: 512,
            budget_chars: 32768,
            num_ctx: 16384,
            ..Default::default()
        };
        let line = status_line(&fresh, "/", 0.0);
        assert!(line.contains("tok est~256"), "{}", line);
    }
}
