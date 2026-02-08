//! Transport bridge module.
//!
//! Provides generic async functions for bridging transport streams with broadcast channels.
//! This module extracts the common stream-handling logic from QUIC server/client implementations
//! to allow reuse with any [`Transport`] implementation.

use bytes::Bytes;
use prost::Message;
use protobuf::{
	control::ControlRequest,
	v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse, TunnelMessage, tunnel_message},
};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	sync::{broadcast, mpsc, oneshot},
};

use crate::control::handler::Handler;
use transport::{BiStream, Transport, TransportError};

/// Maximum size for session init messages (1KB).
pub const SESSION_INIT_MTU: usize = 1024;

/// Maximum size for tunnel messages (2KB).
const TUNNEL_MTU: usize = 2000;

/// Maximum size for control messages (4KB).
const CONTROL_MTU: usize = 4096;

/// Runs the incoming data handler.
///
/// Accepts unidirectional streams from the transport, decodes [`TunnelMessage`]s,
/// and routes them to the appropriate broadcast channel (instructions or responses).
///
/// If `exit_hello_tx` is provided, the first `ExitNodeHello` received will be sent
/// through it. This allows the caller to wait for identity before proceeding.
///
/// # Cancellation Safety
///
/// This function is cancellation safe. If cancelled between stream accepts,
/// no data is lost.
pub async fn run_incoming_data<T: Transport>(
	transport: &T,
	instructions_tx: &broadcast::Sender<EntryNodeInstruction>,
	responses_tx: &broadcast::Sender<ExitNodeResponse>,
	mut exit_hello_tx: Option<oneshot::Sender<ExitNodeHello>>,
	pong_tx: Option<&mpsc::Sender<protobuf::v2::Pong>>,
) -> Result<(), TransportError> {
	let mut read_buf = Vec::with_capacity(TUNNEL_MTU);
	loop {
		let Some(mut recv) = transport.accept_uni().await? else {
			tracing::debug!("Transport closed, stopping incoming data handler");
			return Ok(());
		};

		// Read length-delimited messages from the persistent stream.
		loop {
			let msg: TunnelMessage =
				match read_length_delimited_buf(&mut recv, TUNNEL_MTU, &mut read_buf).await {
					Ok(m) => m,
					Err(e) => {
						// Stream closed or error — accept next stream
						tracing::trace!("Stream ended or error: {e}");
						break;
					}
				};

			match msg.message {
				Some(tunnel_message::Message::ExitNodeResponse(resp)) => {
					tracing::trace!("Received ExitNodeResponse from peer");
					if responses_tx.send(resp).is_err() {
						tracing::warn!(
							"No receivers for ExitNodeResponse - response dropped! (receivers={})",
							responses_tx.receiver_count()
						);
					}
				}
				Some(tunnel_message::Message::EntryNodeInstruction(instr)) => {
					tracing::trace!("Received EntryNodeInstruction from peer");
					if instructions_tx.send(instr).is_err() {
						tracing::warn!(
							"No receivers for EntryNodeInstruction - instruction dropped! (receivers={})",
							instructions_tx.receiver_count()
						);
					}
				}
				Some(tunnel_message::Message::RawPacket(pkt)) => {
					tracing::warn!("Unhandled RawPacket message: {} bytes", pkt.data.len());
				}
				Some(tunnel_message::Message::ExitNodeHello(hello)) => {
					tracing::info!(
						"Received ExitNodeHello: id={}, version={}",
						hello.exit_id,
						hello.version
					);
					if let Some(tx) = exit_hello_tx.take() {
						let _ = tx.send(hello);
					}
				}
				Some(tunnel_message::Message::Ping(ping)) => {
					tracing::trace!("Received Ping, sending Pong");
					let pong_msg = TunnelMessage {
						message: Some(tunnel_message::Message::Pong(protobuf::v2::Pong {
							timestamp_ms: ping.timestamp_ms,
						})),
					};

					match transport.open_uni().await {
						Ok(mut send) => {
							if let Err(e) = write_length_delimited(&mut send, &pong_msg).await {
								tracing::warn!("Failed to write Pong: {e}");
							}
							let _ = send.shutdown().await;
						}
						Err(e) => {
							tracing::warn!("Failed to open stream for Pong: {e}");
						}
					}
				}
				Some(tunnel_message::Message::Pong(pong)) => {
					tracing::trace!("Received Pong");
					if let Some(tx) = pong_tx {
						let _ = tx.send(pong).await;
					}
				}
				None => {
					tracing::warn!("Received TunnelMessage with no message type");
				}
			}
		}
	}
}

/// Runs the outgoing instructions handler (for Host role).
///
/// Opens a single persistent unidirectional stream and sends all instructions
/// using length-delimited framing. This avoids per-message stream open/close
/// overhead.
///
/// # Cancellation Safety
///
/// This function is cancellation safe. If cancelled, no partially-sent messages
/// will be left on the wire.
pub async fn run_outgoing_instructions<T: Transport>(
	transport: &T,
	instructions_tx: &broadcast::Sender<EntryNodeInstruction>,
) -> Result<(), TransportError> {
	let mut rx = instructions_tx.subscribe();
	let mut buf = Vec::with_capacity(TUNNEL_MTU);
	let mut send = transport.open_uni().await?;

	loop {
		let instruction = match rx.recv().await {
			Ok(i) => i,
			Err(broadcast::error::RecvError::Closed) => {
				tracing::debug!("Instructions channel closed");
				let _ = send.shutdown().await;
				return Ok(());
			}
			Err(broadcast::error::RecvError::Lagged(n)) => {
				tracing::warn!("Instructions channel lagged by {n}");
				continue;
			}
		};

		tracing::trace!("Sending EntryNodeInstruction to peer");

		let tunnel_msg = TunnelMessage::from(instruction);
		if let Err(e) = write_length_delimited_buf(&mut send, &tunnel_msg, &mut buf).await {
			tracing::error!("Failed to write instruction: {e}");
			return Err(e);
		}
	}
}

/// Runs the outgoing responses handler
///
/// Opens a single persistent unidirectional stream and sends all responses
/// using length-delimited framing. This avoids per-message stream open/close
/// overhead.
///
/// # Cancellation Safety
///
/// This function is cancellation safe. If cancelled, no partially-sent messages
/// will be left on the wire.
pub async fn run_outgoing_responses<T: Transport>(
	transport: &T,
	responses_tx: &broadcast::Sender<ExitNodeResponse>,
) -> Result<(), TransportError> {
	let mut rx = responses_tx.subscribe();
	let mut buf = Vec::with_capacity(TUNNEL_MTU);
	let mut send = transport.open_uni().await?;

	loop {
		let response = match rx.recv().await {
			Ok(r) => r,
			Err(broadcast::error::RecvError::Closed) => {
				tracing::debug!("Responses channel closed");
				let _ = send.shutdown().await;
				return Ok(());
			}
			Err(broadcast::error::RecvError::Lagged(n)) => {
				tracing::warn!("Responses channel lagged by {n}");
				continue;
			}
		};

		tracing::trace!("Sending ExitNodeResponse to peer");

		let tunnel_msg = TunnelMessage::from(response);
		if let Err(e) = write_length_delimited_buf(&mut send, &tunnel_msg, &mut buf).await {
			tracing::error!("Failed to write response to transport: {e}");
			return Err(e);
		}
	}
}

/// Runs the control request handler.
///
/// Accepts bidirectional streams for control requests, processes them with the
/// provided handler, and sends back responses.
///
/// # Cancellation Safety
///
/// This function is cancellation safe. If cancelled between stream accepts,
/// no data is lost.
pub async fn run_control_handler<T: Transport>(
	transport: &T,
	handler: &Handler,
) -> Result<(), TransportError> {
	loop {
		let Some(mut bi_stream) = transport.accept_bi().await? else {
			tracing::debug!("Transport closed, stopping control handler");
			return Ok(());
		};

		// Read control request
		let mut buf = Vec::with_capacity(CONTROL_MTU);
		match (&mut bi_stream)
			.take(CONTROL_MTU as u64)
			.read_to_end(&mut buf)
			.await
		{
			Ok(0) => {
				tracing::trace!("Control stream closed by peer");
				continue;
			}
			Ok(_) => {}
			Err(e) => {
				tracing::warn!("Error reading control request: {e}");
				continue;
			}
		}

		// Decode and handle request
		let request = match ControlRequest::decode(Bytes::from(buf)) {
			Ok(req) => req,
			Err(e) => {
				tracing::warn!("Failed to decode control request: {e}");
				continue;
			}
		};

		tracing::trace!("Received control request: {:?}", request);
		let response = handler.handle(request);

		// Encode response
		let response_bytes = response.encode_to_vec();

		// Send response
		if let Err(e) = bi_stream.write_all(&response_bytes).await {
			tracing::warn!("Failed to send control response: {e}");
			continue;
		}

		if let Err(e) = bi_stream.finish().await {
			tracing::trace!("Failed to finish control stream: {e}");
		}
	}
}

/// Read a length-delimited protobuf from the stream.
///
/// # Errors
///
/// Returns an error if the stream closes unexpectedly or decoding fails.
pub async fn read_length_delimited<M: Message + Default, S: tokio::io::AsyncRead + Unpin>(
	stream: &mut S,
	max_len: usize,
) -> Result<M, TransportError> {
	read_length_delimited_buf(stream, max_len, &mut Vec::new()).await
}

/// Read a length-delimited protobuf from the stream, reusing the provided buffer.
pub async fn read_length_delimited_buf<M: Message + Default, S: tokio::io::AsyncRead + Unpin>(
	stream: &mut S,
	max_len: usize,
	buf: &mut Vec<u8>,
) -> Result<M, TransportError> {
	let len = stream
		.read_u32()
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	let len = usize::try_from(len).map_err(|_| TransportError::stream("length overflow"))?;
	if len > max_len {
		return Err(TransportError::stream("length exceeds maximum"));
	}
	buf.clear();
	buf.resize(len, 0);
	stream
		.read_exact(buf)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	M::decode(&buf[..]).map_err(|e| TransportError::stream(e.to_string()))
}

/// Write a length-delimited protobuf to the stream.
///
/// Uses a caller-provided buffer to avoid per-call allocation. Falls back to
/// an internal buffer when `None` is passed.
///
/// # Errors
///
/// Returns an error if encoding or writing fails.
pub async fn write_length_delimited<M: Message, S: tokio::io::AsyncWrite + Unpin>(
	stream: &mut S,
	msg: &M,
) -> Result<(), TransportError> {
	write_length_delimited_buf(stream, msg, &mut Vec::new()).await
}

/// Write a length-delimited protobuf, reusing the provided buffer.
pub async fn write_length_delimited_buf<M: Message, S: tokio::io::AsyncWrite + Unpin>(
	stream: &mut S,
	msg: &M,
	buf: &mut Vec<u8>,
) -> Result<(), TransportError> {
	buf.clear();
	msg.encode(buf)
		.map_err(|e| TransportError::stream(e.to_string()))?;
	let len = u32::try_from(buf.len()).map_err(|_| TransportError::stream("length overflow"))?;
	stream
		.write_u32(len)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	stream
		.write_all(buf)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	stream
		.flush()
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::net::SocketAddr;
	use tokio::{
		io::{DuplexStream, duplex},
		sync::mpsc as tokio_mpsc,
	};

	/// A minimal mock transport for testing bridge functions.
	///
	/// Uses `tokio::io::duplex` streams routed through mpsc channels to simulate
	/// a multiplexed transport. Each `open_uni()` on one side creates a duplex
	/// pair and sends the read half to the other side's `accept_uni()`.
	struct MockTransport {
		outgoing_tx: tokio_mpsc::UnboundedSender<DuplexStream>,
		incoming_rx: tokio::sync::Mutex<tokio_mpsc::UnboundedReceiver<DuplexStream>>,
	}

	impl MockTransport {
		fn pair() -> (Self, Self) {
			let (a_tx, a_rx) = tokio_mpsc::unbounded_channel();
			let (b_tx, b_rx) = tokio_mpsc::unbounded_channel();
			(
				Self {
					outgoing_tx: b_tx,
					incoming_rx: tokio::sync::Mutex::new(a_rx),
				},
				Self {
					outgoing_tx: a_tx,
					incoming_rx: tokio::sync::Mutex::new(b_rx),
				},
			)
		}
	}

	struct MockBiStream(DuplexStream);

	impl tokio::io::AsyncRead for MockBiStream {
		fn poll_read(
			mut self: std::pin::Pin<&mut Self>,
			cx: &mut std::task::Context<'_>,
			buf: &mut tokio::io::ReadBuf<'_>,
		) -> std::task::Poll<std::io::Result<()>> {
			std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
		}
	}

	impl tokio::io::AsyncWrite for MockBiStream {
		fn poll_write(
			mut self: std::pin::Pin<&mut Self>,
			cx: &mut std::task::Context<'_>,
			buf: &[u8],
		) -> std::task::Poll<std::io::Result<usize>> {
			std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
		}
		fn poll_flush(
			mut self: std::pin::Pin<&mut Self>,
			cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<std::io::Result<()>> {
			std::pin::Pin::new(&mut self.0).poll_flush(cx)
		}
		fn poll_shutdown(
			mut self: std::pin::Pin<&mut Self>,
			cx: &mut std::task::Context<'_>,
		) -> std::task::Poll<std::io::Result<()>> {
			std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
		}
	}

	impl transport::BiStream for MockBiStream {
		fn finish(
			&mut self,
		) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
			async { Ok(()) }
		}
	}

	impl transport::Transport for MockTransport {
		type SendStream = DuplexStream;
		type RecvStream = DuplexStream;
		type BiStream = MockBiStream;

		fn open_uni(
			&self,
		) -> impl std::future::Future<Output = Result<Self::SendStream, TransportError>> + Send {
			async {
				let (writer, reader) = duplex(64 * 1024);
				self.outgoing_tx
					.send(reader)
					.map_err(|_| TransportError::stream("peer closed"))?;
				Ok(writer)
			}
		}
		fn open_bi(
			&self,
		) -> impl std::future::Future<Output = Result<Self::BiStream, TransportError>> + Send {
			async { Err(TransportError::stream("not implemented")) }
		}
		fn accept_uni(
			&self,
		) -> impl std::future::Future<Output = Result<Option<Self::RecvStream>, TransportError>> + Send
		{
			async {
				let mut rx = self.incoming_rx.lock().await;
				Ok(rx.recv().await)
			}
		}
		fn accept_bi(
			&self,
		) -> impl std::future::Future<Output = Result<Option<Self::BiStream>, TransportError>> + Send
		{
			async { Err(TransportError::stream("not implemented")) }
		}
		fn close(&self) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
			async { Ok(()) }
		}
		fn remote_addr(&self) -> Option<SocketAddr> {
			None
		}
	}

	/// Test that `run_incoming_data` correctly receives an `ExitNodeHello` sent
	/// with length-delimited framing on a per-message stream, matching the
	/// pattern used by QUIC and WS clients.
	#[tokio::test]
	async fn test_hello_received_via_incoming_data() {
		let (sender, receiver) = MockTransport::pair();

		let (instructions_tx, _) = broadcast::channel::<EntryNodeInstruction>(16);
		let (responses_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
		let (hello_tx, hello_rx) = oneshot::channel::<ExitNodeHello>();

		let recv_handle = tokio::spawn(async move {
			run_incoming_data(
				&receiver,
				&instructions_tx,
				&responses_tx,
				Some(hello_tx),
				None,
			)
			.await
		});

		// Send hello using length-delimited framing on a short-lived stream.
		let hello = TunnelMessage {
			message: Some(tunnel_message::Message::ExitNodeHello(ExitNodeHello {
				exit_id: "test-exit".to_string(),
				version: "1.0.0".to_string(),
				auth_token: String::new(),
			})),
		};
		let mut send = sender.open_uni().await.unwrap();
		write_length_delimited(&mut send, &hello).await.unwrap();
		send.shutdown().await.unwrap();

		let received = tokio::time::timeout(std::time::Duration::from_secs(2), hello_rx)
			.await
			.expect("timed out waiting for hello")
			.expect("hello channel closed");
		assert_eq!(received.exit_id, "test-exit");
		assert_eq!(received.version, "1.0.0");

		drop(sender);
		let _ = tokio::time::timeout(std::time::Duration::from_secs(1), recv_handle).await;
	}

	/// Test that multiple data messages flow correctly through a persistent
	/// stream with length-delimited framing.
	#[tokio::test]
	async fn test_data_messages_via_persistent_stream() {
		let (sender, receiver) = MockTransport::pair();

		let (instructions_tx, _) = broadcast::channel::<EntryNodeInstruction>(16);
		let (responses_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
		let mut responses_rx = responses_tx.subscribe();

		let recv_handle = tokio::spawn(async move {
			run_incoming_data(&receiver, &instructions_tx, &responses_tx, None, None).await
		});

		// Send multiple responses on one persistent stream.
		let mut send = sender.open_uni().await.unwrap();
		let mut buf = Vec::new();
		for _ in 0..3 {
			let msg = TunnelMessage::from(ExitNodeResponse::default());
			write_length_delimited_buf(&mut send, &msg, &mut buf)
				.await
				.unwrap();
		}

		for _ in 0..3 {
			tokio::time::timeout(std::time::Duration::from_secs(2), responses_rx.recv())
				.await
				.expect("timed out")
				.expect("channel error");
		}

		drop(sender);
		let _ = tokio::time::timeout(std::time::Duration::from_secs(1), recv_handle).await;
	}

	/// End-to-end: `run_outgoing_responses` → transport → `run_incoming_data`.
	#[tokio::test]
	async fn test_outgoing_to_incoming_roundtrip() {
		let (exit_transport, entry_transport) = MockTransport::pair();

		let (responses_src_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
		let (instructions_dst_tx, _) = broadcast::channel::<EntryNodeInstruction>(16);
		let (responses_dst_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
		let mut responses_dst_rx = responses_dst_tx.subscribe();

		let outgoing = tokio::spawn({
			let responses_src_tx = responses_src_tx.clone();
			async move { run_outgoing_responses(&exit_transport, &responses_src_tx).await }
		});

		let incoming = tokio::spawn(async move {
			run_incoming_data(
				&entry_transport,
				&instructions_dst_tx,
				&responses_dst_tx,
				None,
				None,
			)
			.await
		});

		// Let spawned tasks start and subscribe to channels.
		tokio::task::yield_now().await;

		for _ in 0..3 {
			responses_src_tx.send(ExitNodeResponse::default()).unwrap();
		}

		for _ in 0..3 {
			tokio::time::timeout(std::time::Duration::from_secs(2), responses_dst_rx.recv())
				.await
				.expect("timed out")
				.expect("channel error");
		}

		drop(responses_src_tx);
		let _ = tokio::time::timeout(std::time::Duration::from_secs(1), outgoing).await;
		let _ = tokio::time::timeout(std::time::Duration::from_secs(1), incoming).await;
	}
}
