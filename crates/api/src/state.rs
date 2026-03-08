//! Shared state for the REST API.

use std::sync::Arc;

use tokio::sync::Mutex;
use wallhack_ipc::client::IpcConnection;

use super::auth::Auth;

/// Shared state for the API server.
#[derive(Clone)]
pub struct State {
    pub(super) ipc: Arc<Mutex<IpcConnection>>,
    pub(super) auth: Auth,
}

impl State {
    /// Create API state with an IPC connection and optional auth.
    #[must_use]
    pub fn new(ipc: IpcConnection, auth: Auth) -> Self {
        Self {
            ipc: Arc::new(Mutex::new(ipc)),
            auth,
        }
    }
}
