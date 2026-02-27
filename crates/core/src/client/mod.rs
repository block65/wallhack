#[allow(clippy::module_inception)] // client::client defines the Client trait; intentional structure
pub mod client;
pub mod config;
#[cfg(feature = "quic")]
pub mod quic;
pub mod tls_config;
#[cfg(feature = "websocket")]
pub mod ws;
