use std::sync::Arc;

use protobuf::v2::{SessionInit, SessionProtocol};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use transport::{BiStream, Transport, TransportError};

use crate::transport::bridge::write_length_delimited;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("transport error: {0}")]
	Transport(#[from] TransportError),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

/// Send a UDP packet through the tunnel and return the response.
///
/// Opens a bi-stream to the exit node, sends the payload, waits for response,
/// and returns the response data to be sent back to the original client.
pub async fn send_udp_packet<T: Transport>(
	transport: Arc<T>,
	target: &str,
	source: &str,
	payload: &[u8],
) -> Result<Vec<u8>, Error> {
	tracing::trace!(
		target,
		source,
		payload_len = payload.len(),
		"Forwarding UDP packet to exit"
	);
	let mut stream = transport.open_bi().await?;
	let init = SessionInit {
		target_addr: target.to_string(),
		source_addr: source.to_string(),
		protocol: SessionProtocol::Udp as i32,
	};
	write_length_delimited(&mut stream, &init).await?;
	stream.write_all(payload).await?;
	stream.finish().await?;

	// Wait for response from exit node
	let mut response = Vec::new();
	stream.read_to_end(&mut response).await?;
	tracing::trace!(
		target,
		response_len = response.len(),
		"UDP response received from exit"
	);
	Ok(response)
}
