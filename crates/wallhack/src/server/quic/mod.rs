use std::{sync::Arc, time::Duration};

use protobuf::{
	control_v2::{ControlMessage, control_message},
	v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse},
};
use quinn::{IdleTimeout, crypto::rustls::QuicServerConfig};
use transport::Transport;

use crate::{
	NodeRole,
	control::{handler::Handler, metrics::Metrics, peers::Registry, routes::RouteTable},
	server::tls::{ALPN_QUIC_HTTP, configure_crypto},
	transport::{bridge, quic::QuicTransport},
};

use super::{
	config::ServerConfig,
	server::{AcceptResult, Server, ServerOptions},
	tls,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("tls config error: {0}")]
	StartTls(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	#[error("tls config error: {0}")]
	Connection(#[from] quinn::ConnectionError),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("server tls error: {0}")]
	ServerTls(#[from] tls::Error),

	#[error("tls error: {0}")]
	Tls(#[from] rustls::Error),

	#[error("quinn bounds error: {0}")]
	Quinn(#[from] quinn::VarIntBoundsExceeded),
}

pub struct QuicServer {
	endpoint: quinn::Endpoint,
	options: ServerOptions,
	fingerprint: String,
	psk: Option<String>,
}

impl Server for QuicServer {
	type Error = Error;
	type Transport = QuicTransport;

	fn try_new(mut config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
		let ca_roots_path = config.tls.as_mut().and_then(|t| t.ca_roots.take());
		let (cert_der, priv_key, fingerprint) = configure_crypto(config.tls)?;

		let mut server_crypto = if let Some(ca_path) = ca_roots_path {
			let roots = tls::load_ca_roots(&ca_path)?;
			let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
				.build()
				.map_err(|e| rustls::Error::General(e.to_string()))?;
			rustls::ServerConfig::builder()
				.with_client_cert_verifier(verifier)
				.with_single_cert(cert_der, priv_key)?
		} else {
			rustls::ServerConfig::builder()
				.with_no_client_auth()
				.with_single_cert(cert_der, priv_key)?
		};

		server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

		let mut server_config =
			quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

		let transport_config =
			Arc::get_mut(&mut server_config.transport).expect("transport config has no other refs");

		let timeout = IdleTimeout::try_from(Duration::from_mins(1))?;
		transport_config.max_idle_timeout(Some(timeout));
		transport_config.keep_alive_interval(Some(Duration::from_secs(10)));

		tracing::trace!("Server Config {:?}", server_config);
		tracing::debug!("will listen on {}", config.listen);

		let endpoint = quinn::Endpoint::server(server_config, config.listen)?;

		tracing::info!("local_addr {:?}", endpoint.local_addr());

		Ok(Self {
			endpoint,
			options,
			fingerprint,
			psk: config.psk,
		})
	}

	async fn accept(
		&mut self,
		_role: NodeRole,
	) -> Result<Option<AcceptResult<Self::Transport>>, Error> {
		tracing::debug!("waiting for next connection...");

		let Some(incoming) = self.endpoint.accept().await else {
			return Err(Error::Io(std::io::Error::other(
				"failed to accept incoming connection",
			)));
		};

		let connection = incoming.await?;
		let remote_addr = crate::normalize_socket_addr(connection.remote_address()).to_string();

		// Wrap connection in transport abstraction
		let transport = Arc::new(QuicTransport::new(connection));

		// Accept first bidi stream — this is the control stream.
		let Some(mut control_stream) = transport.accept_bi().await.map_err(|e| {
			std::io::Error::other(format!("failed to accept control bidi stream: {e}"))
		})?
		else {
			return Err(Error::Io(std::io::Error::other(
				"transport closed before control stream accepted",
			)));
		};

		// Read the first message — must be a ControlMessage::Hello (with timeout).
		let hello_result = tokio::time::timeout(
			Duration::from_secs(10),
			bridge::read_length_delimited::<ControlMessage, _>(
				&mut control_stream,
				bridge::CONTROL_MTU,
			),
		)
		.await;

		let exit_hello: Option<ExitNodeHello> = match hello_result {
			Ok(Ok(msg)) => match msg.message {
				Some(control_message::Message::Hello(hello)) => {
					tracing::info!(
						"Received Hello: name={}, version={}",
						hello.name,
						hello.version,
					);
					Some(hello)
				}
				other => {
					tracing::warn!("Expected Hello as first control message, got: {other:?}");
					None
				}
			},
			Ok(Err(e)) => {
				tracing::warn!("Failed to read Hello from control stream: {e}");
				None
			}
			Err(_elapsed) => {
				tracing::warn!("Timed out waiting for Hello on control stream");
				None
			}
		};

		// Get or create shared metrics
		let metrics = self
			.options
			.metrics
			.clone()
			.unwrap_or_else(|| Arc::new(Metrics::default()));

		let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(65536);
		let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(65536);

		// Create control channel for injecting outgoing control messages
		let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

		// Spawn control stream task with handler
		let handler_config = self.options.handler_config.clone();
		let metrics_ctrl = Arc::clone(&metrics);
		let peers_ctrl = self
			.options
			.peers
			.clone()
			.unwrap_or_else(|| Arc::new(Registry::new()));
		let routes_ctrl = self
			.options
			.routes
			.clone()
			.unwrap_or_else(RouteTable::shared);

		tokio::spawn(async move {
			let handler = Handler::new(handler_config, metrics_ctrl, peers_ctrl, routes_ctrl);
			let exit = bridge::run_control_loop(
				&mut control_stream,
				&mut control_rx,
				Some(&handler),
				None, // Hello already read above
				None, // pong handled inline
				None, // server doesn't issue ControlRequests
				Duration::from_secs(30),
			)
			.await;
			tracing::debug!("Control stream finished: {exit:?}");
		});

		// Data tasks are NOT spawned here — the caller does that after PSK validation.
		Ok(Some(AcceptResult::with_exit_hello(
			Arc::clone(&transport),
			(instructions, responses),
			remote_addr,
			metrics,
			exit_hello,
			control_tx,
		)))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		self.endpoint.close(0_u32.into(), b"server stopping");
		tracing::info!("QUIC server endpoint close initiated.");
		Ok(())
	}

	fn fingerprint(&self) -> &str {
		&self.fingerprint
	}

	fn psk(&self) -> Option<&str> {
		self.psk.as_deref()
	}

	fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
		self.endpoint.local_addr()
	}
}
