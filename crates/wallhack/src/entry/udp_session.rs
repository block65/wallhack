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

/// Result of forwarding a UDP packet through the tunnel.
#[derive(Debug)]
pub enum UdpForwardResult {
	/// Exit received a UDP response.
	Response(Vec<u8>),
	/// ICMP Destination Port Unreachable.
	PortUnreachable,
	/// ICMP Destination Host Unreachable.
	HostUnreachable,
	/// ICMP Destination Network Unreachable.
	NetUnreachable,
	/// No response within the timeout window.
	Timeout,
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
) -> Result<UdpForwardResult, Error> {
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

	// Parse status prefix from exit node:
	// Empty = timeout, 0x00+data = success, 0x01 = port unreachable,
	// 0x02 = host unreachable, 0x03 = network unreachable
	let result = if response.is_empty() {
		UdpForwardResult::Timeout
	} else {
		match response[0] {
			0x00 => {
				let data = response[1..].to_vec();
				tracing::trace!(
					target,
					response_len = data.len(),
					"UDP response received from exit"
				);
				UdpForwardResult::Response(data)
			}
			0x01 => UdpForwardResult::PortUnreachable,
			0x02 => UdpForwardResult::HostUnreachable,
			0x03 => UdpForwardResult::NetUnreachable,
			_ => {
				// Unknown status byte — treat as legacy response (no prefix)
				tracing::trace!(
					target,
					response_len = response.len(),
					"UDP response received from exit (legacy)"
				);
				UdpForwardResult::Response(response)
			}
		}
	};
	Ok(result)
}
