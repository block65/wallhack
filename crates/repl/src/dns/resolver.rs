use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use super::resolvable::ResolvableAddress;

#[cfg(feature = "dns-resolver")]
use hickory_resolver::{
	Resolver,
	config::{NameServerConfig, ResolverConfig},
	name_server::TokioConnectionProvider,
	proto::xfer::Protocol,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[cfg(feature = "dns-resolver")]
	#[error("Resolver error: {0}")]
	DnsResolution(#[from] hickory_resolver::ResolveError),

	#[error("IO error: {0}")]
	SocketAddr(#[from] std::io::Error),

	#[error("Invalid address: {0}")]
	InvalidAddress(String),

	#[error("Invalid port: {0}")]
	InvalidPort(String),

	#[error("No records found for {0}")]
	NoRecordsFound(String),

	#[error("Custom DNS server requires the 'dns-resolver' feature")]
	FeatureNotEnabled,
}

/// Resolves a hostname to a `SocketAddr` using either a custom DNS server or
/// the system DNS resolver.
///
/// # Arguments
///
/// * `resolvable` - A `ResolvableAddress` containing the hostname and port to
///   resolve.
/// * `dns_server` - An optional `SocketAddr` specifying a custom DNS server to
///   use. Requires the `dns-resolver` feature.
///
/// # Errors
///
/// This function will return an error if:
/// - The hostname cannot be parsed as an IP address or resolved via DNS.
/// - The custom DNS server fails to resolve the hostname.
/// - The system DNS resolver fails to resolve the hostname.
/// - A custom DNS server is specified but the `dns-resolver` feature is disabled.
pub async fn resolve(
	resolvable: ResolvableAddress,
	dns_server: Option<SocketAddr>,
) -> Result<SocketAddr, Error> {
	let host = &resolvable.hostname;
	let port = resolvable.port;

	// Attempt to parse the host as an IP address first
	if let Ok(ip_addr) = host.parse::<IpAddr>() {
		return Ok(SocketAddr::new(ip_addr, port));
	}

	// If not an IP, then it's a hostname that needs resolution
	let resolved_ip: IpAddr = if let Some(dns_server_addr) = dns_server {
		#[cfg(feature = "dns-resolver")]
		{
			resolve_with_custom_dns(host, dns_server_addr).await?
		}
		#[cfg(not(feature = "dns-resolver"))]
		{
			resolve_with_custom_dns(host, dns_server_addr)?
		}
	} else {
		// Use system DNS resolver
		tracing::debug!("Using system DNS resolver for: {}", resolvable.input);
		let mut addrs_iter = resolvable.input.to_socket_addrs()?;
		addrs_iter
			.next()
			.ok_or_else(|| Error::NoRecordsFound(format!("No records found for hostname: {host}")))?
			.ip()
	};

	Ok(SocketAddr::new(resolved_ip, port))
}

#[cfg(feature = "dns-resolver")]
async fn resolve_with_custom_dns(host: &str, dns_server_addr: SocketAddr) -> Result<IpAddr, Error> {
	tracing::debug!("Using custom DNS server: {dns_server_addr}");

	let mut resolver_config = ResolverConfig::default();

	let nameserver_config = NameServerConfig {
		socket_addr: dns_server_addr,
		protocol: Protocol::Udp,
		trust_negative_responses: true,
		bind_addr: None,
		tls_dns_name: None,
		http_endpoint: None,
	};

	resolver_config.add_name_server(nameserver_config);

	let resolver = Resolver::builder_with_config(
		ResolverConfig::default(),
		TokioConnectionProvider::default(),
	)
	.build();

	let response = resolver.lookup_ip(host).await?;
	response
		.iter()
		.next()
		.ok_or_else(|| Error::NoRecordsFound(format!("No records found for hostname: {host}")))
}

#[cfg(not(feature = "dns-resolver"))]
fn resolve_with_custom_dns(
	_host: &str,
	_dns_server_addr: SocketAddr,
) -> Result<IpAddr, Error> {
	Err(Error::FeatureNotEnabled)
}
