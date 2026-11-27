//! Cancellation state shared by the REPL, the GUI server and the agent loop: at the input
//! prompt Ctrl+C terminates the program, while a turn is running it only cancels that turn.

use std::sync::atomic::{AtomicBool, Ordering};

/// True while the REPL waits for the user to type a line.
static AT_PROMPT: AtomicBool = AtomicBool::new(true);
/// Set while a turn runs; checked at safe points by the agent.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Install a Ctrl+C handler. `on_quit` is called when Ctrl+C arrives at the prompt; while a
/// turn runs the press only cancels it. Must be called once, from the interactive CLI only:
/// `ask` keeps the default behaviour where Ctrl+C terminates immediately.
pub fn install<F>(on_quit: F) -> anyhow::Result<()>
where
    F: Fn() + Send + 'static,
{
    ctrlc::set_handler(move || {
        if AT_PROMPT.load(Ordering::SeqCst) {
            on_quit();
            return;
        }
        if cancel() {
            eprintln!("\n[Ctrl+C] stopping the current turn… (Ctrl+C at the prompt quits)");
        }
    })
    .map_err(|e| anyhow::anyhow!("failed to install the Ctrl+C handler: {}", e))
}

/// Enter prompt mode (and clear any cancellation left over from the previous turn).
pub fn prompt_mode() {
    CANCEL.store(false, Ordering::SeqCst);
    AT_PROMPT.store(true, Ordering::SeqCst);
}

/// Mark the start of work (a model turn or a slash command) so Ctrl+C cancels instead of exits.
pub fn begin_turn() {
    CANCEL.store(false, Ordering::SeqCst);
    AT_PROMPT.store(false, Ordering::SeqCst);
}

/// Mark the end of work and return to the prompt.
pub fn end_turn() {
    AT_PROMPT.store(true, Ordering::SeqCst);
    CANCEL.store(false, Ordering::SeqCst);
}

/// Ask the agent loop to stop at its next safe point. Returns true the first time only.
pub fn cancel() -> bool {
    !CANCEL.swap(true, Ordering::SeqCst)
}

/// Whether the user asked to cancel the current turn.
pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_scoped_to_a_turn() {
        prompt_mode();
        assert!(!cancelled());
        begin_turn();
        assert!(!cancelled());
        assert!(cancel());
        assert!(!cancel());
        assert!(cancelled());
        end_turn();
        assert!(!cancelled());
        assert!(AT_PROMPT.load(Ordering::SeqCst));
    }
}
