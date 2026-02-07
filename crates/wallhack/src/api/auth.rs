//! Authentication for the REST API.

use std::collections::HashMap;

use axum::{
	body::Body,
	http::{Request, StatusCode, header},
	middleware::Next,
	response::{IntoResponse, Response},
};

/// API authentication configuration.
#[derive(Debug, Clone, Default)]
pub struct Auth {
	/// Username for basic auth (None = no auth required)
	pub username: Option<String>,
	/// Password for basic auth
	pub password: Option<String>,
}

impl Auth {
	/// Create auth config with credentials.
	#[must_use]
	pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
		Self {
			username: Some(username.into()),
			password: Some(password.into()),
		}
	}

	/// Check if auth is enabled.
	#[must_use]
	pub fn is_enabled(&self) -> bool {
		self.username.is_some() && self.password.is_some()
	}

	/// Check if auth is configured (alias for [`is_enabled`](Self::is_enabled)).
	#[must_use]
	pub fn is_configured(&self) -> bool {
		self.is_enabled()
	}

	/// Validate credentials.
	#[must_use]
	pub fn validate(&self, username: &str, password: &str) -> bool {
		match (&self.username, &self.password) {
			(Some(u), Some(p)) => u == username && p == password,
			_ => true, // No auth configured = allow all
		}
	}
}

/// Basic auth middleware.
pub async fn middleware(auth: Auth, req: Request<Body>, next: Next) -> Response {
	// Skip auth if not configured
	if !auth.is_enabled() {
		return next.run(req).await;
	}

	// Extract Authorization header
	let auth_header = req
		.headers()
		.get(header::AUTHORIZATION)
		.and_then(|h| h.to_str().ok());

	let Some(auth_header) = auth_header else {
		return unauthorized();
	};

	// Parse "Basic <base64>"
	if !auth_header.starts_with("Basic ") {
		return unauthorized();
	}

	let encoded = &auth_header[6..];
	let Ok(decoded) = base64_decode(encoded) else {
		return unauthorized();
	};

	let Ok(credentials) = String::from_utf8(decoded) else {
		return unauthorized();
	};

	// Parse "username:password"
	let Some((username, password)) = credentials.split_once(':') else {
		return unauthorized();
	};

	if auth.validate(username, password) {
		next.run(req).await
	} else {
		unauthorized()
	}
}

fn unauthorized() -> Response {
	(
		StatusCode::UNAUTHORIZED,
		[(header::WWW_AUTHENTICATE, "Basic realm=\"wallhack\"")],
		"Unauthorized",
	)
		.into_response()
}

#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
	let alphabet: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
		.chars()
		.collect();
	let lookup: HashMap<char, u8> = alphabet
		.iter()
		.enumerate()
		.map(|(i, &c)| (c, i as u8))
		.collect();

	let input: Vec<char> = input.chars().filter(|&c| c != '=').collect();
	let mut output = Vec::new();

	for chunk in input.chunks(4) {
		let mut n: u32 = 0;
		for (i, &c) in chunk.iter().enumerate() {
			let val = *lookup.get(&c).ok_or(())?;
			n |= u32::from(val) << (18 - 6 * i);
		}

		output.push((n >> 16) as u8);
		if chunk.len() > 2 {
			output.push((n >> 8) as u8);
		}
		if chunk.len() > 3 {
			output.push(n as u8);
		}
	}

	Ok(output)
}
