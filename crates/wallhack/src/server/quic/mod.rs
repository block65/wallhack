use std::{sync::Arc, time::Duration};

use protobuf::v2::{EntryNodeInstruction, ExitNodeResponse};
use quinn::{IdleTimeout, crypto::rustls::QuicServerConfig};

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
}

impl Server for QuicServer {
	type Error = Error;
	type Transport = QuicTransport;

	fn try_new(config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
		let (cert_der, priv_key) = configure_crypto(config.tls)?;

		let mut server_crypto = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(cert_der, priv_key)?;

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

		Ok(Self { endpoint, options })
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
		let remote_addr = connection.remote_address().to_string();

		// Wrap connection in transport abstraction
		let transport = Arc::new(QuicTransport::new(connection));

		// Get or create shared metrics
		let metrics = self
			.options
			.metrics
			.clone()
			.unwrap_or_else(|| Arc::new(Metrics::default()));

		let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(65536);
		let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(65536);

		// Create oneshot channel for ExitNodeHello
		let (exit_hello_tx, exit_hello_rx) = tokio::sync::oneshot::channel();

		// Task 0: Control stream handler
		let transport_ctrl = Arc::clone(&transport);
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
			if let Err(e) = bridge::run_control_handler(&*transport_ctrl, &handler).await {
				tracing::debug!("Control handler finished: {e}");
			}
		});

		// Task 1: Incoming data handler is now spawned by the caller (connection handler)
		// so it can inject pong channels for latency measurement.
		Ok(Some(AcceptResult::with_exit_hello(
			Arc::clone(&transport),
			(instructions, responses),
			remote_addr,
			metrics,
			exit_hello_tx,
			exit_hello_rx,
		)))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		self.endpoint.close(0_u32.into(), b"server stopping");
		tracing::info!("QUIC server endpoint close initiated.");
		Ok(())
	}
}
