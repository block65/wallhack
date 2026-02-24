//! WebSocket transport with yamux multiplexing.

pub mod adapter;
pub mod driver;
pub mod streams;
pub mod transport;
pub mod upgrade;

pub use adapter::WebSocketByteStream;
pub use driver::{Driver, TransportStream};
pub use streams::{WebSocketBiStream, WebSocketRecvStream, WebSocketSendStream};
pub use transport::{WebSocketTransport, WebSocketTransportConfig};
pub use upgrade::{UpgradeError, UpgradeResult, upgrade};
