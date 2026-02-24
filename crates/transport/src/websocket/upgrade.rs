//! WebSocket upgrade handler.
//!
//! Provides server-side WebSocket upgrade functionality with minimal
//! dependencies.

use std::io;

use base64::Engine;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// The WebSocket GUID used in the Sec-WebSocket-Accept calculation.
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Maximum size of the HTTP request we'll accept (8KB).
const MAX_REQUEST_SIZE: usize = 8192;

/// Errors that can occur during WebSocket upgrade.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpgradeError {
	#[error("io error: {0}")]
	Io(#[from] io::Error),

	#[error("invalid HTTP request: {0}")]
	InvalidRequest(String),

	#[error("missing required header: {0}")]
	MissingHeader(String),

	#[error("not a WebSocket upgrade request")]
	NotWebSocket,

	#[error("request too large")]
	RequestTooLarge,
}

/// Result of a successful WebSocket upgrade.
#[derive(Debug)]
pub struct UpgradeResult {
	/// The requested path (e.g., "/ws").
	pub path: String,
	/// The Host header value.
	pub hostname: Option<String>,
}

/// Performs the server-side WebSocket upgrade handshake.
///
/// Reads the HTTP upgrade request from the stream, validates it, and sends back
/// the appropriate HTTP 101 Switching Protocols response.
///
/// # Errors
///
/// Returns an error if the request is invalid or not a WebSocket upgrade.
pub async fn upgrade<S>(stream: &mut S) -> Result<UpgradeResult, UpgradeError>
where
	S: AsyncRead + AsyncWrite + Unpin,
{
	let mut reader = BufReader::new(&mut *stream);
	let mut request_data = Vec::with_capacity(1024);
	let mut total_read = 0;

	// Read until we see the end of headers (\r\n\r\n)
	loop {
		let bytes_read = reader.read_until(b'\n', &mut request_data).await?;
		if bytes_read == 0 {
			return Err(UpgradeError::InvalidRequest("connection closed".into()));
		}
		total_read += bytes_read;

		if total_read > MAX_REQUEST_SIZE {
			return Err(UpgradeError::RequestTooLarge);
		}

		// Check if we've reached the end of headers
		if request_data.ends_with(b"\r\n\r\n") || request_data.ends_with(b"\n\n") {
			break;
		}
	}

	// Parse the request
	let request_str = String::from_utf8_lossy(&request_data);
	let mut lines = request_str.lines();

	// Parse request line
	let request_line = lines
		.next()
		.ok_or_else(|| UpgradeError::InvalidRequest("empty request".into()))?;

	let parts: Vec<&str> = request_line.split_whitespace().collect();
	if parts.len() < 3 {
		return Err(UpgradeError::InvalidRequest(
			"malformed request line".into(),
		));
	}

	let method = parts[0];
	let path = parts[1];

	if method != "GET" {
		return Err(UpgradeError::InvalidRequest(format!(
			"expected GET, got {method}"
		)));
	}

	// Parse headers
	let mut ws_key = None;
	let mut host = None;
	let mut is_upgrade = false;
	let mut is_websocket = false;
	let mut is_version_13 = false;

	for line in lines {
		if line.is_empty() {
			break;
		}

		let Some((name, value)) = line.split_once(':') else {
			continue;
		};

		let name = name.trim().to_ascii_lowercase();
		let value = value.trim();

		match name.as_str() {
			"sec-websocket-key" => {
				ws_key = Some(value.to_string());
			}
			"host" => {
				host = Some(value.to_string());
			}
			"upgrade" => {
				is_upgrade = value.eq_ignore_ascii_case("websocket");
			}
			"connection" => {
				is_websocket = value
					.split(',')
					.any(|v| v.trim().eq_ignore_ascii_case("upgrade"));
			}
			"sec-websocket-version" => {
				is_version_13 = value == "13";
			}
			_ => {}
		}
	}

	// Validate WebSocket upgrade requirements
	if !is_upgrade || !is_websocket {
		return Err(UpgradeError::NotWebSocket);
	}

	if !is_version_13 {
		return Err(UpgradeError::InvalidRequest(
			"unsupported websocket version (expected 13)".into(),
		));
	}

	let ws_key = ws_key.ok_or_else(|| UpgradeError::MissingHeader("Sec-WebSocket-Key".into()))?;

	// Calculate the accept key
	let accept_key = compute_accept_key(&ws_key);

	// Send the upgrade response
	let response = format!(
		"HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept_key}\r\n\
         \r\n"
	);

	stream.write_all(response.as_bytes()).await?;
	stream.flush().await?;

	Ok(UpgradeResult {
		path: path.to_string(),
		hostname: host,
	})
}

/// Computes the Sec-WebSocket-Accept value from the client's key.
fn compute_accept_key(key: &str) -> String {
	let mut hasher = Sha1::new();
	hasher.update(key.as_bytes());
	hasher.update(WEBSOCKET_GUID.as_bytes());
	let hash = hasher.finalize();
	base64::engine::general_purpose::STANDARD.encode(hash)
}

#[cfg(test)]
mod tests {
	use tokio::io::AsyncWriteExt;

	use super::*;

	#[test]
	fn test_compute_accept_key() {
		// Example from RFC 6455
		let key = "dGhlIHNhbXBsZSBub25jZQ==";
		let accept = compute_accept_key(key);
		assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
	}

	/// Sends `request` to the server half of a duplex and returns the upgrade result.
	async fn do_upgrade(request: &str) -> Result<UpgradeResult, UpgradeError> {
		let (mut client, mut server) = tokio::io::duplex(65_536);
		let bytes = request.as_bytes().to_vec();
		tokio::spawn(async move {
			client.write_all(&bytes).await.ok();
		});
		upgrade(&mut server).await
	}

	#[tokio::test]
	async fn test_request_too_large() {
		// A header that pushes the total past MAX_REQUEST_SIZE (8192 bytes).
		let filler = "X-Filler: ".to_string() + &"a".repeat(8200) + "\r\n";
		let request = format!("GET /ws HTTP/1.1\r\nHost: localhost\r\n{filler}\r\n");
		let err = do_upgrade(&request).await.unwrap_err();
		assert!(
			matches!(err, UpgradeError::RequestTooLarge),
			"unexpected error: {err}"
		);
	}

	#[tokio::test]
	async fn test_missing_sec_websocket_key() {
		let request = "GET /ws HTTP/1.1\r\n\
		               Host: localhost\r\n\
		               Upgrade: websocket\r\n\
		               Connection: Upgrade\r\n\
		               Sec-WebSocket-Version: 13\r\n\
		               \r\n";
		let err = do_upgrade(request).await.unwrap_err();
		assert!(
			matches!(err, UpgradeError::MissingHeader(_)),
			"unexpected error: {err}"
		);
	}

	#[tokio::test]
	async fn test_non_get_method() {
		let request = "POST /ws HTTP/1.1\r\n\
		               Host: localhost\r\n\
		               Upgrade: websocket\r\n\
		               Connection: Upgrade\r\n\
		               Sec-WebSocket-Version: 13\r\n\
		               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
		               \r\n";
		let err = do_upgrade(request).await.unwrap_err();
		assert!(
			matches!(err, UpgradeError::InvalidRequest(_)),
			"unexpected error: {err}"
		);
	}

	#[tokio::test]
	async fn test_wrong_websocket_version() {
		let request = "GET /ws HTTP/1.1\r\n\
		               Host: localhost\r\n\
		               Upgrade: websocket\r\n\
		               Connection: Upgrade\r\n\
		               Sec-WebSocket-Version: 8\r\n\
		               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
		               \r\n";
		let err = do_upgrade(request).await.unwrap_err();
		assert!(
			matches!(err, UpgradeError::InvalidRequest(_)),
			"unexpected error: {err}"
		);
	}

	#[tokio::test]
	async fn test_not_websocket_upgrade() {
		// Missing Upgrade/Connection headers → NotWebSocket
		let request = "GET /ws HTTP/1.1\r\n\
		               Host: localhost\r\n\
		               Sec-WebSocket-Version: 13\r\n\
		               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
		               \r\n";
		let err = do_upgrade(request).await.unwrap_err();
		assert!(
			matches!(err, UpgradeError::NotWebSocket),
			"unexpected error: {err}"
		);
	}
}
