//! Error types for the daemon engine.

/// Typed errors for node operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
	/// Required transport feature not compiled in.
	#[error("{0} transport not available (compile with --features {0})")]
	TransportUnavailable(&'static str),

	/// Invalid configuration.
	#[error("{0}")]
	Config(String),

	/// PSK authentication failure.
	#[error("PSK authentication failed for peer {0}")]
	PskAuth(String),

	/// Control channel unexpectedly closed.
	#[error("control channel closed")]
	ChannelClosed,

	/// Address resolution produced no results.
	#[error("no addresses resolved for {0}")]
	NoAddresses(String),

	/// Address parse error.
	#[error(transparent)]
	AddrParse(#[from] std::net::AddrParseError),

	#[error("TUN subsystem error: {0}")]
	TunActor(#[from] wallhack_core::entry::actor::Error),

	#[error("connection manager error: {0}")]
	ConnectionManager(#[from] wallhack_core::entry::manager::Error),

	#[error("runtime task error: {0}")]
	Runtime(#[from] tokio::task::JoinError),

	/// I/O error.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// WebSocket Server Error
	#[error(transparent)]
	WebSocketServer(#[from] wallhack_core::server::ws::Error),

	/// DNS resolution failure.
	#[error("DNS resolution failed: {0}")]
	DnsResolution(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Transport creation or connection failure.
	#[error("transport error: {0}")]
	Transport(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Stream-level I/O error (bi-stream read/write).
	#[error("stream error: {0}")]
	Stream(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl NodeError {
	/// Whether this error is transient and the operation can be retried.
	///
	/// Returns `false` for "dead" errors that will never succeed:
	/// TLS/crypto failures, fingerprint mismatch, PSK auth, missing features.
	#[must_use]
	pub fn is_retryable(&self) -> bool {
		match self {
			Self::Transport(e) => !is_dead_transport_error(e.as_ref()),
			Self::PskAuth(_) | Self::TransportUnavailable(_) | Self::Config(_) => false,
			_ => true,
		}
	}
}

/// Returns `true` if the error is "dead" — will never succeed on retry.
///
/// Dead errors: TLS/crypto handshake failures, fingerprint mismatch, certificate errors.
/// Transient errors: timeouts, connection refused, reset, IO.
///
/// Walks the error source chain, downcasting to detect:
/// - QUIC crypto errors (`quinn::ConnectionError::TransportError` with code `0x100..0x200`)
/// - rustls errors anywhere in the chain (WebSocket path wraps them in `std::io::Error`)
/// - rustls errors inside `std::io::Error` via `get_ref()` (tokio-rustls construction path)
fn is_dead_transport_error(err: &(dyn std::error::Error + 'static)) -> bool {
	let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
	while let Some(e) = current {
		// QUIC path: TLS alerts map to QUIC crypto error codes 0x100..0x200.
		// Fingerprint mismatch, CertificateRequired, HandshakeFailure all land here.
		if let Some(quinn::ConnectionError::TransportError(te)) =
			e.downcast_ref::<quinn::ConnectionError>()
		{
			let code = u64::from(te.code);
			if (0x100..0x200).contains(&code) {
				return true;
			}
		}

		// WebSocket path: tokio_rustls wraps rustls::Error inside std::io::Error.
		// std::io::Error boxes its inner error, and .source() may not always
		// penetrate it depending on construction. Use get_ref() explicitly.
		if let Some(io_err) = e.downcast_ref::<std::io::Error>()
			&& let Some(inner) = io_err.get_ref()
			&& inner.is::<quinn::rustls::Error>()
		{
			return true;
		}

		// Direct rustls::Error anywhere in the source chain.
		if e.downcast_ref::<quinn::rustls::Error>().is_some() {
			return true;
		}

		current = e.source();
	}
	false
}
