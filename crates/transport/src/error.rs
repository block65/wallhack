//! Transport layer errors.

use std::io;

/// Errors that can occur during transport operations.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TransportError {
	/// I/O error from the underlying transport.
	#[error("io error: {0}")]
	Io(#[from] io::Error),

	/// The connection was closed.
	#[error("connection closed: {0}")]
	ConnectionClosed(String),

	/// Stream-level error (read/write failed).
	#[error("stream error: {0}")]
	Stream(String),

	/// Protocol-level error (invalid framing, etc.).
	#[error("protocol error: {0}")]
	Protocol(String),

	/// The operation timed out.
	#[error("operation timed out")]
	Timeout,
}

impl TransportError {
	/// Creates a connection closed error with the given reason.
	#[must_use]
	pub fn connection_closed(reason: impl Into<String>) -> Self {
		Self::ConnectionClosed(reason.into())
	}

	/// Creates a stream error with the given message.
	#[must_use]
	pub fn stream(msg: impl Into<String>) -> Self {
		Self::Stream(msg.into())
	}

	/// Creates a protocol error with the given message.
	#[must_use]
	pub fn protocol(msg: impl Into<String>) -> Self {
		Self::Protocol(msg.into())
	}
}
