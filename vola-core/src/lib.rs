//! vola-core: the agent engine shared by the CLI (`vola-agent`) and the native GUI
//! (`vola-gui`).
//!
//! The crate is synchronous on purpose: model requests are blocking HTTP calls, tools run on
//! the calling thread, and the WebSocket server in [`server`] multiplexes a reader loop with
//! worker threads instead of an async runtime.

pub mod agent;
pub mod commands;
pub mod config;
pub mod interrupt;
pub mod llm;
pub mod memory;
pub mod prompt;
pub mod protocol;
pub mod scripts;
pub mod server;
pub mod skills;
pub mod tools;
pub mod util;
pub mod ws;

pub use config::Config;
