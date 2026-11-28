//! Interactive REPL: slash commands plus natural-language chat.

use anyhow::Result;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vola_core::agent::{Agent, TurnProgress};
use vola_core::commands;

pub fn run(mut agent: Agent) -> Result<()> {
    if let Err(e) = vola_core::interrupt::install(|| {
        crate::console::restore();
        std::process::exit(130);
    }) {
        eprintln!("[warn] {}", e);
    }
    banner(&agent);
    // Animate only for a real interactive session: piped stdin has no echo that would put
    // the cursor on a fresh line, so a status line would overwrite the prompt.
    let animated = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        vola_core::interrupt::prompt_mode();
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
        vola_core::interrupt::begin_turn();
        if input.starts_with('/') {
            match commands::execute(&mut agent, &input) {
                Ok(output) => {
                    if output.clear {
                        print!("\x1b[2J\x1b[3J\x1b[H");
                        std::io::stdout().flush().ok();
                    }
                    if !output.text.is_empty() {
                        println!("{}", output.text);
                    }
                    vola_core::interrupt::end_turn();
                    if output.quit {
                        break;
                    }
                }
                Err(e) => {
                    println!("[command failed] {:#}", e);
                    vola_core::interrupt::end_turn();
                }
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
        vola_core::interrupt::end_turn();
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
