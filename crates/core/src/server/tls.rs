use std::{fmt::Write, fs, io, path::Path};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::config::TlsConfig;

pub const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

/// Compute the SHA-256 fingerprint of a DER-encoded certificate.
///
/// Returns a string in the format `sha256:<hex>`.
#[must_use]
pub fn cert_fingerprint(cert_der: &[u8]) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, cert_der);
    let mut hex = String::with_capacity(hash.as_ref().len() * 2);
    for b in hash.as_ref() {
        let _ = write!(hex, "{b:02x}");
    }
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

/// Load CA root certificates from a PEM or DER file into a `RootCertStore`.
///
/// Used to build a client certificate verifier for mTLS.
pub fn load_ca_roots(path: &Path) -> Result<rustls::RootCertStore, Error> {
    let data = fs::read(path)?;
    let mut store = rustls::RootCertStore::empty();

    if path.extension().is_some_and(|ext| ext == "der") {
        store.add(CertificateDer::from(data))?;
    } else {
        for cert in rustls_pemfile::certs(&mut data.as_slice()) {
            store.add(cert?)?;
        }
    }

    Ok(store)
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
        // No SANs, no identifying CN — generic self-signed cert that doesn't
        // leak tool identity to network observers or IDS.
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        // Default validity runs to year 4096 — conspicuously unusual.
        // 90 days matches Let's Encrypt's standard validity window.
        let now = time::OffsetDateTime::now_utc();
        let expiry = now + time::Duration::days(90);
        params.not_before = now;
        params.not_after = expiry;
        let key_pair = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key_pair)?;
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
        let cert_der = CertificateDer::from(cert.der().to_vec());
        (vec![cert_der], key_der)
    };

    let fingerprint = cert_fingerprint(certs[0].as_ref());

    Ok((certs, key, fingerprint))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::NamedTempFile;

    use super::*;

    fn generate_ca_cert() -> (rcgen::Certificate, rcgen::KeyPair) {
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key_pair).unwrap();
        (cert, key_pair)
    }

    #[test]
    fn load_ca_roots_pem_single_cert() {
        let (cert, _) = generate_ca_cert();
        let pem = cert.pem();

        let mut file = NamedTempFile::with_suffix(".pem").unwrap();
        file.write_all(pem.as_bytes()).unwrap();

        let store = load_ca_roots(file.path()).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn load_ca_roots_pem_multiple_certs() {
        let (cert1, _) = generate_ca_cert();
        let (cert2, _) = generate_ca_cert();
        let pem = format!("{}{}", cert1.pem(), cert2.pem());

        let mut file = NamedTempFile::with_suffix(".pem").unwrap();
        file.write_all(pem.as_bytes()).unwrap();

        let store = load_ca_roots(file.path()).unwrap();
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn load_ca_roots_der() {
        let (cert, _) = generate_ca_cert();
        let der = cert.der().to_vec();

        let mut file = NamedTempFile::with_suffix(".der").unwrap();
        file.write_all(&der).unwrap();

        let store = load_ca_roots(file.path()).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn load_ca_roots_missing_file() {
        let result = load_ca_roots(Path::new("/nonexistent/ca.pem"));
        assert!(matches!(result, Err(Error::CertificateLoad(_))));
    }

    #[test]
    fn load_ca_roots_empty_pem() {
        let file = NamedTempFile::with_suffix(".pem").unwrap();
        let store = load_ca_roots(file.path()).unwrap();
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn build_client_verifier_succeeds_without_crls() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let (cert, _) = generate_ca_cert();
        let pem = cert.pem();
        let mut file = NamedTempFile::with_suffix(".pem").unwrap();
        file.write_all(pem.as_bytes()).unwrap();

        let store = load_ca_roots(file.path()).unwrap();
        let result =
            rustls::server::WebPkiClientVerifier::builder(std::sync::Arc::new(store)).build();
        assert!(result.is_ok(), "build() failed: {:?}", result.err());
    }
}
