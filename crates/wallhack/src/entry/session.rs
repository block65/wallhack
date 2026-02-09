use std::sync::Arc;

use netstack::async_stack::tcp_stream::TcpStream;
use protobuf::v2::{ResponseStatus, SessionInit, SessionProtocol, SessionStatus};
use tokio::io::copy_bidirectional;
use transport::{BiStream, Transport, TransportError};

use crate::transport::bridge::{read_length_delimited, write_length_delimited};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("transport error: {0}")]
	Transport(#[from] TransportError),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

pub async fn run_tcp_session<D, T>(mut local: TcpStream<D>, transport: Arc<T>) -> Result<(), Error>
where
	D: smoltcp::phy::Device + Send + 'static,
	T: Transport + 'static,
{
	// In AnyIP mode, smoltcp accepts connections destined for any IP.
	// local_endpoint = the destination the client wanted (e.g., 10.200.2.10:9999)
	// remote_endpoint = the client's source address (e.g., 10.200.1.10:54016)
	let target = local
		.local_endpoint()
		.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotConnected, "missing local"))?;
	let source = local
		.remote_endpoint()
		.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotConnected, "missing remote"))?;
	tracing::debug!(?target, ?source, "TCP session starting, opening bi-stream");
	let mut remote = transport.open_bi().await?;
	tracing::debug!(?target, ?source, "bi-stream opened, sending init");
	let init = SessionInit {
		target_addr: target.to_string(),
		source_addr: source.to_string(),
		protocol: SessionProtocol::Tcp as i32,
	};
	write_length_delimited(&mut remote, &init).await?;

	// Wait for exit node to confirm the connection succeeded before copying data.
	// Without this, smoltcp has already SYN-ACKed the client but we don't know
	// if the real target is reachable. On failure, dropping `local` sends RST.
	let status: SessionStatus =
		read_length_delimited(&mut remote, crate::transport::bridge::SESSION_INIT_MTU).await?;
	if status.status() != ResponseStatus::Success {
		tracing::debug!(?target, status = ?status.status(), reason = %status.reason, "exit rejected connection");
		return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, status.reason).into());
	}

	tracing::debug!(?target, ?source, "exit confirmed, starting copy_bidirectional");

	match copy_bidirectional(&mut local, &mut remote).await {
		Ok((to_remote, to_local)) => {
			tracing::debug!(?target, to_remote, to_local, "copy_bidirectional completed");
		}
		Err(e) => {
			tracing::debug!(?target, error = %e, "copy_bidirectional failed");
			return Err(e.into());
		}
	}
	let _ = remote.finish().await;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::transport::bridge::SESSION_INIT_MTU;

	/// Verify that a success SessionStatus round-trips correctly.
	#[tokio::test]
	async fn session_status_success_round_trip() {
		let (mut writer, mut reader) = tokio::io::duplex(1024);

		let status = SessionStatus {
			status: ResponseStatus::Success.into(),
			reason: String::new(),
		};
		write_length_delimited(&mut writer, &status).await.unwrap();
		drop(writer);

		let read_status: SessionStatus =
			read_length_delimited(&mut reader, SESSION_INIT_MTU).await.unwrap();
		assert_eq!(read_status.status(), ResponseStatus::Success);
	}

	/// Verify that a refused SessionStatus round-trips with the reason intact.
	#[tokio::test]
	async fn session_status_refused_round_trip() {
		let (mut writer, mut reader) = tokio::io::duplex(1024);

		let status = SessionStatus {
			status: ResponseStatus::ConnectionRefused.into(),
			reason: "Connection refused".to_string(),
		};
		write_length_delimited(&mut writer, &status).await.unwrap();
		drop(writer);

		let read_status: SessionStatus =
			read_length_delimited(&mut reader, SESSION_INIT_MTU).await.unwrap();
		assert_eq!(read_status.status(), ResponseStatus::ConnectionRefused);
		assert_eq!(read_status.reason, "Connection refused");
	}
}
