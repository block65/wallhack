use smoltcp::socket::{tcp, udp};

/// Errors produced by the netstack crate.
///
/// # Variants
///
/// Each variant represents a distinct failure mode of the network stack.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The specified port is invalid (e.g. port 0).
	#[error("invalid port: {port}")]
	InvalidPort { port: u16 },

	/// A TCP listen operation failed.
	#[error("listen failed: {0}")]
	Listen(#[from] tcp::ListenError),

	/// A TCP send operation failed.
	#[error("send failed: {0}")]
	Send(#[from] tcp::SendError),

	/// A TCP recv operation failed.
	#[error("recv failed: {0}")]
	Recv(#[from] tcp::RecvError),

	/// A UDP bind operation failed.
	#[error("udp bind failed: {0}")]
	UdpBind(#[from] udp::BindError),

	/// A UDP send operation failed.
	#[error("udp send failed: {0}")]
	UdpSend(#[from] udp::SendError),

	/// A UDP recv operation failed.
	#[error("udp recv failed: {0}")]
	UdpRecv(#[from] udp::RecvError),

	/// The socket is in an unexpected state for the requested operation.
	#[error("invalid socket state: {0}")]
	InvalidState(tcp::State),

	/// The socket handle does not refer to a valid socket.
	#[error("invalid socket handle")]
	InvalidHandle,

	/// The maximum number of concurrent sockets has been reached.
	#[error("max concurrent sockets reached")]
	MaxSocketsReached,
}
