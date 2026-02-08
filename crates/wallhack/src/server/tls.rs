use std::{fs, io};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::config::TlsConfig;

pub const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

/// Compute the SHA-256 fingerprint of a DER-encoded certificate.
///
/// Returns a string in the format `sha256:<hex>`.
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
	let hash = ring::digest::digest(&ring::digest::SHA256, cert_der);
	let hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
	format!("sha256:{hex}")
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("No private key found in PEM file {0}")]
	NoPrivateKeyFound(io::Error),

	#[error("{0}")]
	CertificateEncoding(#[from] rustls::pki_types::pem::Error),

	// rcgen::RcgenError
	#[error("{0}")]
	CertificateGeneration(#[from] rcgen::Error),

	// rustls::Error
	#[error("{0}")]
	Tls(#[from] rustls::Error),

	// std::io::Error
	#[error("{0}")]
	CertificateLoad(#[from] std::io::Error),
}

pub fn configure_crypto(
	config: Option<TlsConfig>,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>, String), Error> {
	let _ = rustls::crypto::ring::default_provider().install_default();

	let (certs, key) = if let Some(config) = config {
		let key_path = &config.key_pem_file;
		let cert_path = &config.cert_pem_file;

		let key_data = fs::read(key_path)?;
		let cert_data = fs::read(cert_path)?;

		let key = if key_path.extension().is_some_and(|ext| ext == "der") {
			PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_data))
		} else {
			rustls_pemfile::private_key(&mut key_data.as_slice())?.ok_or_else(|| {
				Error::NoPrivateKeyFound(io::Error::new(
					io::ErrorKind::InvalidData,
					"No private key found",
				))
			})?
		};

		let certs = if matches!(cert_path.extension(), Some(ext) if ext == "der") {
			vec![CertificateDer::from(cert_data)]
		} else {
			rustls_pemfile::certs(&mut cert_data.as_slice()).collect::<Result<Vec<_>, _>>()?
		};
		(certs, key)
	} else {
		tracing::trace!("generating self-signed certificate");
		let cert = rcgen::generate_simple_self_signed([env!("CARGO_PKG_NAME").into()])?;
		let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
		let cert_der = CertificateDer::from(cert.cert.der().to_vec());
		(vec![cert_der], key_der)
	};

	let fingerprint = cert_fingerprint(certs[0].as_ref());

	Ok((certs, key, fingerprint))
}
