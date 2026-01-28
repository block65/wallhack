use smoltcp::socket::tcp;

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

	/// The socket is in an unexpected state for the requested operation.
	#[error("invalid socket state: {0}")]
	InvalidState(tcp::State),

	/// The socket handle does not refer to a valid socket.
	#[error("invalid socket handle")]
	InvalidHandle,
}
