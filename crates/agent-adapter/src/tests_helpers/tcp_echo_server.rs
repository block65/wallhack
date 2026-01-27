use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::Instrument;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("IO error: {}", .0)]
	Io(#[from] std::io::Error),
}

#[derive(Debug, PartialEq)]
enum EchoResult {
	Continue,
	TryClose,
}

#[tracing::instrument]
pub async fn run(addr: SocketAddr) -> Result<(), Error> {
	let listener = tokio::net::TcpListener::bind(&addr).await?;

	tracing::info!("Listening on {addr}");

	// connection loop
	loop {
		let (mut stream, peer_addr) = listener.accept().await?;
		tracing::debug!("Accepted connection from {peer_addr}. spawning task");

		tokio::spawn(
			async move {
				let mut buf = [0; 1024];
				// Loop to continuously read data from the stream
				let result = loop {
					match stream.read(&mut buf).await {
						Ok(0) => {
							// A result of Ok(0) means the client has closed their side of the connection.
							tracing::debug!("0 byte read. Graceful close by peer: {peer_addr}");
							break EchoResult::Continue;
						}
						Ok(n) => {
							tracing::trace!("Received {n} bytes from {peer_addr}",);

							match stream.write_all(&buf[..n]).await {
								Ok(()) => {
									tracing::trace!("Echoing {n} bytes to {peer_addr}",);
								}
								Err(e) => {
									tracing::error!("Error echoing data to {peer_addr}: {e}");
									break EchoResult::TryClose;
								}
							}
						}
						Err(e) => {
							tracing::error!("Error reading from stream from {peer_addr}: {e}");
							break EchoResult::Continue;
						}
					}
				};

				// Attempt to gracefully shutdown the stream's write side.
				// If this call is here, it's after each successful echo.
				if result == EchoResult::TryClose
					&& let Err(e) = stream.shutdown().await
				{
					match e.kind() {
						std::io::ErrorKind::NotConnected | std::io::ErrorKind::BrokenPipe => {
							tracing::warn!(
								peer_addr = %peer_addr,
								error = %e,
								"Stream shutdown failed as connection was likely closed by peer post-echo"
							);
						}
						_ => {
							tracing::warn!(
								peer_addr = %peer_addr,
								error = %e,
								"Error shutting down stream"
							);
						}
					}
				}
			}
			.instrument(tracing::debug_span!("echo_task", peer_addr = %peer_addr)),
		);
	}
}
