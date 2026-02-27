#[cfg(feature = "quic")]
mod create;

pub mod config;
#[cfg(feature = "quic")]
pub mod quic;
#[allow(clippy::module_inception)] // server::server defines the Server trait; intentional structure
pub mod server;
pub mod tls;
#[cfg(feature = "websocket")]
pub mod ws;

#[cfg(feature = "quic")]
pub use create::{Error, create};
