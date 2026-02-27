// Internal APIs where error types are self-documenting.
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::too_many_lines)]
#![warn(unused_extern_crates)]

pub mod adapter;
pub mod session;
pub mod session_key;
pub mod sessions;

pub use wallhack_wire::SocketSet;
