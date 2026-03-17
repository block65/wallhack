//! Shared state for the REST API.

use std::sync::Arc;

use axum::http::{HeaderValue, Method};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use wallhack_ipc::client::IpcConnection;

use super::auth::Auth;

/// CORS policy for the API.
#[derive(Clone, Debug)]
pub enum CorsPolicy {
    /// Allow requests from any `localhost` / `127.0.0.1` / `[::1]` origin
    /// regardless of port. Default when no explicit origin is configured.
    Localhost,
    /// Allow requests from a specific origin (e.g. `https://dashboard.example.com`).
    Origin(String),
}

// Localhost is the safe default — allows dev UIs without exposing the API.
#[allow(clippy::derivable_impls)] // REASON: explicit default documents the security decision
impl Default for CorsPolicy {
    fn default() -> Self {
        Self::Localhost
    }
}

impl CorsPolicy {
    /// Convert into a `tower_http::cors::CorsLayer`.
    ///
    /// # Panics
    ///
    /// Panics if [`CorsPolicy::Origin`] contains a value that is not a valid HTTP header.
    pub fn into_layer(self) -> CorsLayer {
        let base = CorsLayer::new()
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
            ])
            .max_age(std::time::Duration::from_hours(1));

        match self {
            Self::Localhost => base.allow_origin(tower_http::cors::AllowOrigin::predicate(
                |origin: &HeaderValue, _req: &axum::http::request::Parts| {
                    let Ok(s) = origin.to_str() else {
                        return false;
                    };
                    let lower = s.to_ascii_lowercase();
                    [
                        "http://localhost:",
                        "https://localhost:",
                        "http://localhost",
                        "https://localhost",
                        "http://127.0.0.1:",
                        "https://127.0.0.1:",
                        "http://[::1]:",
                        "https://[::1]:",
                    ]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
                },
            )),
            Self::Origin(origin) => base.allow_origin(
                origin
                    .parse::<HeaderValue>()
                    .expect("configured CORS origin must be a valid header value"),
            ),
        }
    }
}

/// Shared state for the API server.
#[derive(Clone)]
pub struct State {
    pub(super) ipc: Arc<Mutex<IpcConnection>>,
    pub(super) auth: Auth,
    pub(super) cors: CorsPolicy,
    pub(super) peer_events:
        tokio::sync::broadcast::Sender<wallhack_core::control::peers::PeerEvent>,
}

impl State {
    /// Create API state with an IPC connection and optional auth.
    #[must_use]
    pub fn new(
        ipc: IpcConnection,
        auth: Auth,
        peer_events: tokio::sync::broadcast::Sender<wallhack_core::control::peers::PeerEvent>,
    ) -> Self {
        Self {
            ipc: Arc::new(Mutex::new(ipc)),
            auth,
            cors: CorsPolicy::default(),
            peer_events,
        }
    }

    /// Set the CORS policy.
    #[must_use]
    pub fn with_cors(mut self, cors: CorsPolicy) -> Self {
        self.cors = cors;
        self
    }
}
