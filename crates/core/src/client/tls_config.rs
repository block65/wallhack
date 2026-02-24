use std::fs;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::{
    server::tls::ALPN_QUIC_HTTP,
    tls::verifiers::{FingerprintVerifier, SkipServerVerification},
};

use super::config::MtlsConfig;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("No private key found in PEM file")]
    NoPrivateKeyFound,

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

fn with_great_danger() -> rustls::ClientConfig {
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth()
}

fn with_client_auth(config: MtlsConfig) -> Result<rustls::ClientConfig, Error> {
    let mut roots = rustls::RootCertStore::empty();

    if let Some(ca_path) = config.ca_roots {
        roots.add(CertificateDer::from(fs::read(ca_path)?))?;
    }

    let key_path = &config.key_pem_file;
    let cert_path = &config.cert_pem_file;

    let key_data = fs::read(key_path)?;
    let cert_data = fs::read(cert_path)?;

    let key = if key_path.extension().is_some_and(|ext| ext == "der") {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_data))
    } else {
        rustls_pemfile::private_key(&mut key_data.as_slice())?.ok_or(Error::NoPrivateKeyFound)?
    };

    let certs = if cert_path.extension().is_some_and(|ext| ext == "der") {
        vec![CertificateDer::from(cert_data)]
    } else {
        rustls_pemfile::certs(&mut cert_data.as_slice()).collect::<Result<Vec<_>, _>>()?
    };

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)?;

    Ok(config)
}

fn with_fingerprint_verification(fingerprint: String) -> rustls::ClientConfig {
    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(FingerprintVerifier::new(fingerprint))
        .with_no_client_auth()
}

pub fn client_config(
    config: Option<MtlsConfig>,
    accept_fingerprint: Option<String>,
) -> Result<rustls::ClientConfig, Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut config = match (config, accept_fingerprint) {
        (Some(config), _) => with_client_auth(config)?,
        (None, Some(fp)) => {
            // Default to sha256 if no hash algorithm prefix is provided
            let fp = if fp.contains(':') {
                fp
            } else {
                format!("sha256:{fp}")
            };
            with_fingerprint_verification(fp)
        }
        (None, None) => with_great_danger(),
    };

    // common config
    config.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    Ok(config)
}
