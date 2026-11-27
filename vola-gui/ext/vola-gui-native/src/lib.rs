//! Native helpers for the Vola GUI: start and stop the `vola-agent serve` core.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use nsh_core::prelude::*;

/// What the GUI imports from `vola-gui-native`.
#[native_module("vola-gui-native")]
fn native(registry: &mut Registry) {
    registry.class::<CoreServer>();
}

/// Starts the vola core server as a child process.
#[class]
pub struct CoreServer {
    child: Option<Child>,
}

#[class]
impl CoreServer {
    /// A handle that has started nothing yet.
    pub fn new() -> Self {
        CoreServer { child: None }
    }

    /// Starts `vola-agent serve --addr <addr>` from the workspace build.
    ///
    /// `program` empty picks `VOLA_AGENT_BIN`, then `target/release/vola-agent`, then
    /// `target/debug/vola-agent`, then `vola-agent` from `PATH`. The child runs in the
    /// repository root so `config.json` is found. The returned line is shown in the UI.
    pub fn start(&mut self, program: String, addr: String) -> String {
        self.stop();
        let root = workspace_root();
        let program = resolve_program(&program, &root);
        let mut command = Command::new(&program);
        command
            .arg("serve")
            .arg("--addr")
            .arg(&addr)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: serving happens headless behind the GUI.
            command.creation_flags(0x0800_0000);
        }
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.child = Some(child);
                format!(
                    "started {} serve --addr {} (pid {})",
                    program.display(),
                    addr,
                    pid
                )
            }
            Err(error) => format!("failed to start {}: {}", program.display(), error),
        }
    }

    /// Stops the server this handle started, if it is still running.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Whether the started server is still running.
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

fn workspace_root() -> PathBuf {
    // ext/vola-gui-native -> ext -> vola-gui -> repository root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_program(program: &str, root: &Path) -> PathBuf {
    if !program.trim().is_empty() {
        return PathBuf::from(program);
    }
    if let Ok(bin) = std::env::var("VOLA_AGENT_BIN") {
        if !bin.trim().is_empty() {
            return PathBuf::from(bin);
        }
    }
    let exe = if cfg!(windows) {
        "vola-agent.exe"
    } else {
        "vola-agent"
    };
    for profile in ["release", "debug"] {
        let candidate = root.join("target").join(profile).join(exe);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("vola-agent")
}
