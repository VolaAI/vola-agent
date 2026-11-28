//! vola-tui: the terminal front-end for vola-agent.
//!
//! It owns the command-line surface (argv parsing, subcommands, the interactive REPL and the
//! Windows console setup) while the engine itself lives in `vola-core`.

pub mod cli;
pub mod console;
pub mod repl;

/// Run the terminal front-end: parse argv, dispatch the subcommand and restore the console.
pub fn main() {
    console::init();
    let result = cli::run();
    console::restore();
    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}
