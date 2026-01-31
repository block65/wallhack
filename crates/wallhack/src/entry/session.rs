use std::sync::Arc;

use netstack::async_stack::tcp_stream::TcpStream;
use protobuf::v2::{SessionInit, SessionProtocol};
use tokio::io::copy_bidirectional;
use transport::{BiStream, Transport, TransportError};

use crate::transport::bridge::write_length_delimited;

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
	let mut remote = transport.open_bi().await?;
	let init = SessionInit {
		target_addr: target.to_string(),
		source_addr: source.to_string(),
		protocol: SessionProtocol::Tcp as i32,
	};
	write_length_delimited(&mut remote, &init).await?;

	let _ = copy_bidirectional(&mut local, &mut remote).await?;
	let _ = remote.finish().await;
	Ok(())
}
