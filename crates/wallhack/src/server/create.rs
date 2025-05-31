use std::{sync::Arc, time::Duration};

use quinn::{IdleTimeout, crypto::rustls::QuicServerConfig};

use crate::server::tls::{ALPN_QUIC_HTTP, Error as ServerTlsError, configure_crypto};

use super::config;

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("tls config error: {0}")]
	StartTls(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("server tls error: {0}")]
	ServerTls(#[from] ServerTlsError),

	#[error("tls error: {0}")]
	Tls(#[from] rustls::Error),

	// quinn::VarIntBoundsExceeded
	#[error("quinn bounds error: {0}")]
	Quinn(#[from] quinn::VarIntBoundsExceeded),

	#[error("{source} (addr {addr})")]
	Endpoint {
		source: std::io::Error,

		addr: std::net::SocketAddr,
	},
}

pub fn create(config: config::ServerConfig) -> Result<quinn::Endpoint, Error> {
	let (cert_der, priv_key) = configure_crypto(config.tls)?;

	// let mut server_config =
	// 	quinn::ServerConfig::with_single_cert(cert_der.clone(), priv_key.clone_key())?;
	// let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
	// transport_config.max_concurrent_uni_streams(0_u8.into());

	let mut server_crypto = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(cert_der, priv_key)?;

	server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

	let mut server_config =
		quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

	let transport_config = Arc::get_mut(&mut server_config.transport).ok_or_else(|| {
		std::io::Error::other("Failed to get mutable reference to transport config")
	})?;

	let timeout = IdleTimeout::try_from(Duration::from_secs(10))?;
	transport_config.max_idle_timeout(Some(timeout));
	transport_config.keep_alive_interval(Some(Duration::from_secs(5)));
	// transport_config.max_concurrent_uni_streams(1_u8.into());

	tracing::trace!("Server Config {:?}", server_config);
	// tracing::trace!("will listen on {}", config.listen);

	let endpoint =
		quinn::Endpoint::server(server_config, config.listen).map_err(|e| Error::Endpoint {
			source: e,
			addr: config.listen,
		})?;

	tracing::debug!("Listening on {:?}", endpoint.local_addr().ok());

	Ok(endpoint)
}
