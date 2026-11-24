//! Ctrl+C routing for the REPL: at the input prompt it terminates the program (the usual
//! CLI behaviour), while the agent is working it only cancels the current turn.

use std::sync::atomic::{AtomicBool, Ordering};

/// True while the REPL waits for the user to type a line.
static AT_PROMPT: AtomicBool = AtomicBool::new(true);
/// Set by the Ctrl+C handler while a turn runs; checked at safe points by the agent.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Install the handler. Must be called once, from the interactive REPL only: `ask` keeps the
/// default behaviour where Ctrl+C terminates immediately.
pub fn install() -> anyhow::Result<()> {
    ctrlc::set_handler(|| {
        if AT_PROMPT.load(Ordering::SeqCst) {
            crate::console::restore();
            std::process::exit(130);
        }
        // Print once even if the user impatiently presses Ctrl+C several times.
        if !CANCEL.swap(true, Ordering::SeqCst) {
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
        CANCEL.store(true, Ordering::SeqCst); // what the Ctrl+C handler does mid-turn
        assert!(cancelled());
        end_turn();
        assert!(!cancelled());
        assert!(AT_PROMPT.load(Ordering::SeqCst));
    }
}
