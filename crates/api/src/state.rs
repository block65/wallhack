//! Shared state for the REST API.

use std::sync::Arc;

use tokio::sync::Mutex;
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
    /// Check whether a request `Origin` header value is allowed.
    #[must_use]
    pub fn is_allowed(&self, origin: &str) -> bool {
        match self {
            Self::Localhost => {
                let lower = origin.to_ascii_lowercase();
                for prefix in [
                    "http://localhost:",
                    "https://localhost:",
                    "http://localhost",
                    "https://localhost",
                    "http://127.0.0.1:",
                    "https://127.0.0.1:",
                    "http://[::1]:",
                    "https://[::1]:",
                ] {
                    if lower.starts_with(prefix) {
                        return true;
                    }
                }
                false
            }
            Self::Origin(allowed) => origin == allowed,
        }
    }
}

/// Shared state for the API server.
#[derive(Clone)]
pub struct State {
    pub(super) ipc: Arc<Mutex<IpcConnection>>,
    pub(super) auth: Auth,
    pub(super) cors: CorsPolicy,
}

impl State {
    /// Create API state with an IPC connection and optional auth.
    #[must_use]
    pub fn new(ipc: IpcConnection, auth: Auth) -> Self {
        Self {
            ipc: Arc::new(Mutex::new(ipc)),
            auth,
            cors: CorsPolicy::default(),
        }
    }

    /// Set the CORS policy.
    #[must_use]
    pub fn with_cors(mut self, cors: CorsPolicy) -> Self {
        self.cors = cors;
        self
    }
}
