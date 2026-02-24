use crate::server::{self, config::ServerConfig};
use std::net::SocketAddr;

pub async fn start_mock_echo_server()
-> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
	let socket_addr = SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), 0);
	let listener = tokio::net::TcpListener::bind(socket_addr).await?;
	let socket_addr = listener.local_addr()?;

	let handle = tokio::spawn(async move {
		while let Ok((socket, _)) = listener.accept().await {
			tokio::spawn(async move {
				let mut buf = vec![0; 1024];
				while socket.readable().await.is_ok() {
					match socket.try_read(&mut buf) {
						Ok(n) => {
							tracing::debug!("Read {} bytes", n);
							if n == 0 {
								tracing::debug!("Connection closed");
								break; // Connection closed
							}
							if socket.writable().await.is_ok() {
								let wn = socket.try_write(&buf[..n]);
								tracing::debug!("Wrote {} bytes", wn.unwrap_or(0));
							}
						}
						Err(_) => break, // Error occurred
					}
				}
			});
		}
	});

	tracing::debug!("echo server listening on: {socket_addr:?}");

	Ok((socket_addr, handle))
}

pub fn create_test_server() -> anyhow::Result<(quinn::Endpoint, SocketAddr)> {
	let server_config = ServerConfig {
		listen: SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), 0),
		tls: None,
		psk: None,
		max_peers: None,
	};
	let server = server::create(server_config)?;
	let endpoint = server.local_addr()?;
	Ok((server, endpoint))
}

