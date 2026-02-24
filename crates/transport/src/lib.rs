//! Transport layer abstraction.
//!
//! This crate provides a transport-agnostic interface for multiplexed
//! connections. The [`Transport`] trait abstracts over different transport
//! mechanisms (QUIC, WebSocket+yamux) allowing the rest of the stack to work
//! with any compatible transport.

#![feature(trait_alias)]

mod error;
pub mod traits;

#[cfg(feature = "quic")]
pub mod quic;

#[cfg(feature = "websocket")]
pub mod websocket;

pub use error::TransportError;
pub use traits::{BiStream, Transport};
