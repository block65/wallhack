use std::{
	fmt::{Display, Formatter},
	net::{IpAddr, SocketAddr},
	str::FromStr,
};

#[derive(Debug, Clone)]
pub struct ResolvableAddress {
	pub input: String,
	pub hostname: String,
	pub port: u16,
}

impl Display for ResolvableAddress {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}:{}", self.hostname, self.port)
	}
}

impl FromStr for ResolvableAddress {
	type Err = String; // Define a suitable error type

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let parts: Vec<&str> = s.split(':').collect();
		if parts.len() != 2 {
			return Err(format!(
				"Address must be in <hostname_or_ip>:<port> format. Got: {s}",
			));
		}

		let host_str = parts[0].to_string();
		if host_str.is_empty() {
			return Err("Address cannot be empty.".to_string());
		}

		let port = parts[1]
			.parse::<u16>()
			.map_err(|e| format!("Invalid port number: {}. Error: {}", parts[1], e))?;

		Ok(ResolvableAddress {
			input: s.to_string(),
			hostname: host_str,
			port,
		})
	}
}

/// Parses a string into a `SocketAddr`.
///
/// # Errors
///
/// Returns an error if the input string is not a valid `SocketAddr` or
/// `IpAddr`.
pub fn parse_str_to_addr(value: &str) -> Result<SocketAddr, String> {
	// Try parsing it as SocketAddr directly (ip:port)
	if let Ok(addr) = value.parse::<SocketAddr>() {
		return Ok(addr);
	}

	// Try parsing a plain IP and use default DNS port 53
	if let Ok(ip) = value.parse::<IpAddr>() {
		return Ok(SocketAddr::new(ip, 53));
	}

	Err(format!(
		"Invalid DNS server address: {value}. Must be <ip> or <ip>:<port>",
	))
}
