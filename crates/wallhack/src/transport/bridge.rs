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
	sync::{broadcast, oneshot},
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
) -> Result<(), TransportError> {
	loop {
		let Some(recv) = transport.accept_uni().await? else {
			tracing::debug!("Transport closed, stopping incoming data handler");
			return Ok(());
		};

		// Read the entire message (limited by MTU)
		let mut buf = Vec::with_capacity(TUNNEL_MTU);
		match recv.take(TUNNEL_MTU as u64).read_to_end(&mut buf).await {
			Ok(0) => {
				tracing::trace!("Stream closed by peer (0 bytes)");
				continue;
			}
			Ok(_) => {}
			Err(e) => {
				tracing::warn!("Error reading from stream: {e}");
				continue;
			}
		}

		tracing::trace!("Received {} bytes from peer", buf.len());

		let msg = match TunnelMessage::decode(Bytes::from(buf)) {
			Ok(m) => m,
			Err(e) => {
				tracing::error!("Failed to decode TunnelMessage: {e}");
				continue;
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
				// Send to oneshot channel if caller is waiting for it
				if let Some(tx) = exit_hello_tx.take() {
					let _ = tx.send(hello);
				}
			}
			Some(tunnel_message::Message::Ping(ping)) => {
				tracing::trace!("Received Ping, sending Pong");
				// Immediately respond with pong
				let pong = TunnelMessage {
					message: Some(tunnel_message::Message::Pong(protobuf::v2::Pong {
						timestamp_ms: ping.timestamp_ms,
					})),
				};
				
				// Encode and send the pong
				match transport.open_uni().await {
					Ok(mut send) => {
						let encoded = pong.encode_to_vec();
						if let Err(e) = send.write_all(&encoded).await {
							tracing::warn!("Failed to write Pong: {e}");
						}
						// Stream closes when dropped
					}
					Err(e) => {
						tracing::warn!("Failed to open stream for Pong: {e}");
					}
				}
			}
			Some(tunnel_message::Message::Pong(_)) => {
				// Pongs are handled by entry node, not by exit
				tracing::trace!("Received Pong (unexpected on exit node)");
			}
			None => {
				tracing::warn!("Received TunnelMessage with no message type");
			}
		}
	}
}

/// Runs the outgoing instructions handler (for Host role).
///
/// Subscribes to the instructions broadcast channel and sends each instruction
/// to the peer over a new unidirectional stream.
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

	loop {
		let instruction = match rx.recv().await {
			Ok(i) => i,
			Err(broadcast::error::RecvError::Closed) => {
				tracing::debug!("Instructions channel closed");
				return Ok(());
			}
			Err(broadcast::error::RecvError::Lagged(n)) => {
				tracing::warn!("Instructions channel lagged by {n}");
				continue;
			}
		};

		tracing::trace!("Sending EntryNodeInstruction to peer");

		let mut send = transport.open_uni().await?;

		let tunnel_msg = TunnelMessage::from(instruction);
		buf.clear();
		if let Err(e) = tunnel_msg.encode(&mut buf) {
			tracing::error!("Failed to encode instruction: {e}");
			continue;
		}

		if let Err(e) = send.write_all(&buf).await {
			tracing::error!("Failed to write instruction: {e}");
			return Err(TransportError::stream(e.to_string()));
		}

		if let Err(e) = send.shutdown().await {
			tracing::trace!("Failed to shutdown stream: {e}");
		}
	}
}

/// Runs the outgoing responses handler
///
/// Subscribes to the responses broadcast channel and sends each response
/// to the peer over a new unidirectional stream.
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

	loop {
		let response = match rx.recv().await {
			Ok(r) => r,
			Err(broadcast::error::RecvError::Closed) => {
				tracing::debug!("Responses channel closed");
				return Ok(());
			}
			Err(broadcast::error::RecvError::Lagged(n)) => {
				tracing::warn!("Responses channel lagged by {n}");
				continue;
			}
		};

		tracing::trace!("Sending ExitNodeResponse to peer");

		let mut send = transport.open_uni().await?;

		let tunnel_msg = TunnelMessage::from(response);
		buf.clear();
		if let Err(e) = tunnel_msg.encode(&mut buf) {
			tracing::error!("Failed to encode response: {e}");
			continue;
		}

		if let Err(e) = send.write_all(&buf).await {
			tracing::error!("Failed to write response to transport: {e}");
			return Err(TransportError::stream(e.to_string()));
		}

		if let Err(e) = send.shutdown().await {
			tracing::trace!("Failed to shutdown stream: {e}");
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
	let len = stream
		.read_u32()
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	let len = usize::try_from(len).map_err(|_| TransportError::stream("length overflow"))?;
	if len > max_len {
		return Err(TransportError::stream("length exceeds maximum"));
	}
	let mut buf = vec![0u8; len];
	stream
		.read_exact(&mut buf)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	M::decode(&buf[..]).map_err(|e| TransportError::stream(e.to_string()))
}

/// Write a length-delimited protobuf to the stream.
///
/// # Errors
///
/// Returns an error if encoding or writing fails.
pub async fn write_length_delimited<M: Message, S: tokio::io::AsyncWrite + Unpin>(
	stream: &mut S,
	msg: &M,
) -> Result<(), TransportError> {
	let mut buf = Vec::new();
	msg.encode(&mut buf)
		.map_err(|e| TransportError::stream(e.to_string()))?;
	let len = u32::try_from(buf.len()).map_err(|_| TransportError::stream("length overflow"))?;
	stream
		.write_u32(len)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	stream
		.write_all(&buf)
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	stream
		.flush()
		.await
		.map_err(|e| TransportError::stream(e.to_string()))?;
	Ok(())
}
