#![warn(unused_extern_crates)]

pub mod cli;
pub mod daemon_cli;
pub mod output;
pub mod subscriber;
pub mod version;

// Re-export from wallhack-ipc so existing consumers (wallhack.rs, repl.rs) keep working.
pub use wallhack_ipc::{client as ipc, framing};

#[cfg(feature = "repl")]
pub mod repl;
