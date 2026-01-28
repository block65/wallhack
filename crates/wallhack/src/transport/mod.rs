//! Transport layer abstraction.
//!
//! Re-exports from the [`transport`] crate and provides the application-level
//! [`bridge`] module for protobuf message routing over transports.

pub mod bridge;

pub use transport::*;
