//! Core transport traits.

use std::{future::Future, net::SocketAddr};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::TransportError;

/// A bidirectional stream combining send and receive capabilities.
pub trait BiStream: AsyncRead + AsyncWrite + Send + Unpin {
    /// Finishes the write half of this stream, signaling no more data will be
    /// sent.
    ///
    /// This is similar to [`AsyncWriteExt::shutdown`] but may have
    /// transport-specific semantics (e.g., QUIC's `finish()` sends a FIN frame).
    fn finish(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

/// A multiplexed transport connection.
///
/// This trait abstracts over different transport mechanisms (QUIC,
/// WebSocket+yamux, etc.) providing a unified interface for opening and
/// accepting streams.
///
/// # Stream Types
///
/// Transports support two types of streams:
/// - **Unidirectional**: One-way streams for sending data (opened via
///   [`open_uni`][Self::open_uni]) or receiving data (accepted via
///   [`accept_uni`][Self::accept_uni]).
/// - **Bidirectional**: Two-way streams for request-response patterns (opened
///   via [`open_bi`][Self::open_bi], accepted via
///   [`accept_bi`][Self::accept_bi]).
pub trait Transport: Send + Sync {
    /// The send stream type for unidirectional outgoing streams.
    type SendStream: AsyncWrite + Send + Unpin;

    /// The receive stream type for unidirectional incoming streams.
    type RecvStream: AsyncRead + Send + Unpin;

    /// The bidirectional stream type.
    type BiStream: BiStream;

    /// Opens a new unidirectional stream for sending data.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed or the transport cannot open
    /// more streams.
    fn open_uni(&self) -> impl Future<Output = Result<Self::SendStream, TransportError>> + Send;

    /// Opens a new bidirectional stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed or the transport cannot open
    /// more streams.
    fn open_bi(&self) -> impl Future<Output = Result<Self::BiStream, TransportError>> + Send;

    /// Accepts an incoming unidirectional stream.
    ///
    /// Returns `None` if the connection has been closed gracefully.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed unexpectedly.
    fn accept_uni(
        &self,
    ) -> impl Future<Output = Result<Option<Self::RecvStream>, TransportError>> + Send;

    /// Accepts an incoming bidirectional stream.
    ///
    /// Returns `None` if the connection has been closed gracefully.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed unexpectedly.
    fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<Option<Self::BiStream>, TransportError>> + Send;

    /// Closes the transport connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be closed cleanly.
    fn close(&self) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Returns the remote address of the peer, if known.
    fn remote_addr(&self) -> Option<SocketAddr>;
}
