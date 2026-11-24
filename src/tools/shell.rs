//! Command execution: timeout, process-tree kill, output truncation, error reporting, dangerous-command blocking.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{Config, ShellKind};
use crate::util::{char_len, truncate_middle};

pub struct CmdOutcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub elapsed: f64,
    pub shell: String,
}

/// Block dangerous commands (disable with VOLA_AGENT_ALLOW_DANGEROUS=1).
pub fn danger_reason(cmd: &str) -> Option<&'static str> {
    let c = cmd.to_ascii_lowercase();
    let checks: &[(&str, &str)] = &[
        ("rm -rf /", "recursively delete the root directory"),
        ("rm -rf /c", "recursively delete an entire drive"),
        ("rm -rf c:", "recursively delete an entire drive"),
        (
            "del /f /s /q c:\\",
            "format-style deletion of the system drive",
        ),
        (
            "rd /s /q c:\\",
            "recursively delete system drive directories",
        ),
        ("format c:", "format the system drive"),
        ("format-volume", "format a volume"),
        ("clear-disk", "wipe a physical disk"),
        ("remove-partition", "delete a partition"),
        ("mkfs", "format a filesystem"),
        ("diskpart", "operate directly on disk partitions"),
        ("bcdedit", "modify the boot configuration"),
        ("reg delete hklm", "delete a system registry key"),
        ("shutdown", "shut down / reboot"),
        ("dd if=/dev/zero of=/dev/", "overwrite a disk device"),
        (":(){", "fork bomb"),
    ];
    for (pat, why) in checks {
        if c.contains(pat) {
            return Some(why);
        }
    }
    None
}

fn shell_candidates(cfg: &Config, requested: Option<&str>) -> ShellKind {
    match requested.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("bash") | Some("sh") | Some("git-bash") => ShellKind::Bash,
        Some("cmd") | Some("cmd.exe") => ShellKind::Cmd,
        Some("powershell") | Some("pwsh") => ShellKind::PowerShell,
        _ => cfg.shell,
    }
}

fn build_command(cfg: &Config, cmd: &str, kind: ShellKind) -> (Command, String) {
    let mut c = match kind {
        ShellKind::Bash => {
            let bash: PathBuf = cfg
                .bash_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("bash"));
            let mut c = Command::new(&bash);
            c.arg("-c").arg(cmd);
            c
        }
        ShellKind::Cmd => {
            let mut c = Command::new("cmd.exe");
            c.arg("/D")
                .arg("/S")
                .arg("/C")
                .arg(format!("chcp 65001>nul & {}", cmd));
            c
        }
        ShellKind::PowerShell => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            // Force UTF-8 for PowerShell's own messages and for native output (curl etc.);
            // otherwise redirected output arrives as GBK and turns into mojibake.
            c.arg(format!(
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8; {}",
                cmd
            ));
            c
        }
    };
    let label = match kind {
        ShellKind::Bash => "bash".to_string(),
        ShellKind::Cmd => "cmd".to_string(),
        ShellKind::PowerShell => "powershell".to_string(),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    (c, label)
}

fn reader<R: Read + Send + 'static>(
    mut r: R,
) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = buf.clone();
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut b) = sink.lock() {
                        // Memory guard: cap a single stream at 4MB
                        if b.len() < 4 * 1024 * 1024 {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    (buf, handle)
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let mut c = Command::new("taskkill.exe");
    c.args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
    let _ = c.status();
}

#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn finish_readers(
    out_buf: &Arc<Mutex<Vec<u8>>>,
    err_buf: &Arc<Mutex<Vec<u8>>>,
    out_h: std::thread::JoinHandle<()>,
    err_h: std::thread::JoinHandle<()>,
) {
    let deadline = Instant::now() + Duration::from_millis(1500);
    while (!out_h.is_finished() || !err_h.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // If the thread is still stuck on a pipe held open by a grandchild, leave it detached and read only what was collected so far.
    let _ = out_buf;
    let _ = err_buf;
    if out_h.is_finished() {
        let _ = out_h.join();
    }
    if err_h.is_finished() {
        let _ = err_h.join();
    }
}

fn snapshot(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let bytes = buf.lock().map(|b| b.clone()).unwrap_or_default();
    crate::util::decode_text(&bytes)
}

/// Run one command and return structured result text (never panics; errors become readable text).
pub fn run(
    cfg: &Config,
    cmd: &str,
    timeout: Duration,
    cwd: Option<&Path>,
    requested_shell: Option<&str>,
) -> Result<CmdOutcome> {
    if let Some(why) = danger_reason(cmd) {
        if !cfg.allow_dangerous {
            anyhow::bail!("refused to run: recognized as a dangerous command ({}). If it is truly needed, the user must run it manually.", why);
        }
    }
    let kind = shell_candidates(cfg, requested_shell);
    let (mut command, label) = build_command(cfg, cmd, kind);
    if let Some(dir) = cwd {
        if !dir.exists() {
            anyhow::bail!("working directory does not exist: {}", dir.display());
        }
        command.current_dir(dir);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {}", label))?;
    let pid = child.id();
    let (out_buf, out_h) = reader(child.stdout.take().context("cannot read stdout")?);
    let (err_buf, err_h) = reader(child.stderr.take().context("cannot read stderr")?);

    let deadline = start + timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut code = None;
    loop {
        if crate::interrupt::cancelled() {
            cancelled = true;
            kill_tree(pid);
            let _ = child.kill();
            if let Ok(status) = child.wait() {
                code = status.code();
            }
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code();
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_tree(pid);
                    let _ = child.kill();
                    if let Ok(status) = child.wait() {
                        code = status.code();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }

    finish_readers(&out_buf, &err_buf, out_h, err_h);
    let stdout_raw = snapshot(&out_buf);
    let stderr_raw = snapshot(&err_buf);
    let elapsed = start.elapsed().as_secs_f64();

    let (stdout, _) = truncate_middle(&stdout_raw, cfg.output_limit);
    let (stderr, _) = truncate_middle(&stderr_raw, cfg.output_limit / 2);

    Ok(CmdOutcome {
        code,
        stdout,
        stderr,
        timed_out,
        cancelled,
        elapsed,
        shell: label,
    })
}

impl CmdOutcome {
    pub fn render(&self, cmd: &str) -> String {
        let mut s = String::new();
        s.push_str(&format!("[command] {}\n", cmd.trim()));
        s.push_str(&format!("[shell] {}\n", self.shell));
        if self.cancelled {
            s.push_str("[exit code] interrupted by the user (Ctrl+C)\n");
        } else if self.timed_out {
            s.push_str("[exit code] killed after the timeout\n");
        } else {
            match self.code {
                Some(0) => s.push_str("[exit code] 0\n"),
                Some(c) => s.push_str(&format!(
                    "[exit code] {} (the command failed, see stderr)\n",
                    c
                )),
                None => s.push_str("[exit code] unknown (process was killed)\n"),
            }
        }
        s.push_str(&format!("[elapsed] {:.2}s\n", self.elapsed));
        if self.cancelled {
            s.push_str("[cancelled] the user pressed Ctrl+C; the whole process tree was killed.\n");
        } else if self.timed_out {
            s.push_str("[timeout] still running past the limit; the whole process tree was killed. Run it in the background, split the command, or raise timeout_secs.\n");
        }
        if !self.stdout.trim().is_empty() {
            s.push_str("\n[stdout]\n");
            s.push_str(self.stdout.trim_end());
            s.push('\n');
        }
        if !self.stderr.trim().is_empty() {
            s.push_str("\n[stderr]\n");
            s.push_str(self.stderr.trim_end());
            s.push('\n');
        }
        if !self.cancelled && self.stdout.trim().is_empty() && self.stderr.trim().is_empty() {
            s.push_str("\n[output] (no output)\n");
            let c = cmd.to_ascii_lowercase();
            if c.contains("curl")
                || c.contains("wget")
                || c.contains("http://")
                || c.contains("https://")
            {
                s.push_str("[hint] the network command printed nothing: keep the URL fully quoted (an unquoted '&' splits the command), add -f/--fail-with-body to surface HTTP errors, and prefer keyless endpoints; never pass a placeholder API key.\n");
            } else {
                s.push_str("[hint] the command printed nothing: it may not exist, may have failed silently (stderr redirected), or may have written to a file.\n");
            }
        }
        s.push_str(&format!(
            "[output bytes] {}\n",
            char_len(&self.stdout) + char_len(&self.stderr)
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_dangerous_patterns() {
        assert!(danger_reason("rm -rf /").is_some());
        assert!(danger_reason("diskpart").is_some());
        assert!(danger_reason("del /f /s /q C:\\").is_some());
        assert!(danger_reason("ls -la").is_none());
        assert!(danger_reason("rm -rf target").is_none());
    }
}
