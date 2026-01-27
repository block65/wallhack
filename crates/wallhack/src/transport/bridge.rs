//! Transport bridge module.
//!
//! Provides generic async functions for bridging transport streams with broadcast channels.
//! This module extracts the common stream-handling logic from QUIC server/client implementations
//! to allow reuse with any [`Transport`] implementation.

use bytes::Bytes;
use prost::Message;
use protobuf::{
	control::ControlRequest,
	v2::{AgentHello, AgentResponse, HostInstruction, TunnelMessage, tunnel_message},
};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	sync::{broadcast, oneshot},
};

use super::{BiStream, Transport, TransportError};
use crate::control::handler::Handler;

/// Maximum size for tunnel messages (2KB).
const TUNNEL_MTU: usize = 2000;

/// Maximum size for control messages (4KB).
const CONTROL_MTU: usize = 4096;

/// Runs the incoming data handler.
///
/// Accepts unidirectional streams from the transport, decodes [`TunnelMessage`]s,
/// and routes them to the appropriate broadcast channel (instructions or responses).
///
/// If `agent_hello_tx` is provided, the first `AgentHello` received will be sent
/// through it. This allows the caller to wait for agent identity before proceeding.
///
/// # Cancellation Safety
///
/// This function is cancellation safe. If cancelled between stream accepts,
/// no data is lost.
pub async fn run_incoming_data<T: Transport>(
	transport: &T,
	instructions_tx: &broadcast::Sender<HostInstruction>,
	responses_tx: &broadcast::Sender<AgentResponse>,
	mut agent_hello_tx: Option<oneshot::Sender<AgentHello>>,
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
			Some(tunnel_message::Message::AgentResponse(resp)) => {
				tracing::trace!("Received AgentResponse from peer");
				if responses_tx.send(resp).is_err() {
					tracing::warn!(
						"No receivers for AgentResponse - response dropped! (receivers={})",
						responses_tx.receiver_count()
					);
				}
			}
			Some(tunnel_message::Message::HostInstruction(instr)) => {
				tracing::trace!("Received HostInstruction from peer");
				if instructions_tx.send(instr).is_err() {
					tracing::warn!(
						"No receivers for HostInstruction - instruction dropped! (receivers={})",
						instructions_tx.receiver_count()
					);
				}
			}
			Some(tunnel_message::Message::RawPacket(pkt)) => {
				tracing::warn!("Unhandled RawPacket message: {} bytes", pkt.data.len());
			}
			Some(tunnel_message::Message::AgentHello(hello)) => {
				tracing::info!(
					"Received AgentHello: id={}, version={}",
					hello.agent_id,
					hello.version
				);
				// Send to oneshot channel if caller is waiting for it
				if let Some(tx) = agent_hello_tx.take() {
					let _ = tx.send(hello);
				}
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
	instructions_tx: &broadcast::Sender<HostInstruction>,
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

		tracing::trace!("Sending HostInstruction to peer");

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

/// Runs the outgoing responses handler (for Agent role).
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
	responses_tx: &broadcast::Sender<AgentResponse>,
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

		tracing::trace!("Sending AgentResponse to peer");

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
