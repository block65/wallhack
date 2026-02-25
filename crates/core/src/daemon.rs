//! Daemon handle for managing a running wallhack node.
//!
//! [`DaemonHandle`] wraps a spawned node task and provides access to the
//! [`NodeApi`] trait for querying and controlling the node from external
//! consumers (IPC, REST API, CLI).

use std::sync::Arc;

use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};

use crate::{
    control::peers::{PeerEvent, SharedRegistry},
    node_api::NodeApi,
};

/// Handle to a running wallhack node.
///
/// Owns the spawned node task and provides access to the [`NodeApi`] trait
/// for management operations. Created by `start_entry()` / `start_exit()` /
/// `start_relay()` in the CLI crate.
pub struct DaemonHandle {
    node_api: Arc<dyn NodeApi>,
    peers: SharedRegistry,
    shutdown_tx: watch::Sender<()>,
    node_task: JoinHandle<anyhow::Result<()>>,
}

impl DaemonHandle {
    /// Creates a new daemon handle.
    #[must_use]
    pub fn new(
        node_api: Arc<dyn NodeApi>,
        peers: SharedRegistry,
        shutdown_tx: watch::Sender<()>,
        node_task: JoinHandle<anyhow::Result<()>>,
    ) -> Self {
        Self {
            node_api,
            peers,
            shutdown_tx,
            node_task,
        }
    }

    /// Returns a reference to the node's management API.
    #[must_use]
    pub fn api(&self) -> &dyn NodeApi {
        &*self.node_api
    }

    /// Returns a cloneable handle to the node's management API.
    #[must_use]
    pub fn api_arc(&self) -> Arc<dyn NodeApi> {
        Arc::clone(&self.node_api)
    }

    /// Returns the shutdown receiver for coordinating graceful shutdown.
    #[must_use]
    pub fn shutdown_rx(&self) -> watch::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Subscribe to peer lifecycle events (connect/disconnect).
    #[must_use]
    pub fn subscribe_peer_events(&self) -> broadcast::Receiver<PeerEvent> {
        self.peers.subscribe()
    }

    /// Returns the peer events sender for passing to subsystems (e.g. IPC listener).
    #[must_use]
    pub fn peer_events_sender(&self) -> broadcast::Sender<PeerEvent> {
        self.peers.events_sender()
    }

    /// Signals the node to shut down and waits for it to finish.
    ///
    /// Sends the shutdown signal first. If the node doesn't stop promptly,
    /// aborts the task.
    ///
    /// # Errors
    ///
    /// Returns error if the node task panicked or returned an error.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.shutdown_tx.send(()).ok();
        self.node_task.abort();
        match self.node_task.await {
            Ok(result) => result,
            Err(e) if e.is_cancelled() => Ok(()),
            Err(e) => Err(anyhow::anyhow!("node task panicked: {e}")),
        }
    }

    /// Waits for the node task to complete without requesting shutdown.
    ///
    /// Blocks until the node exits on its own (e.g. REPL quit, connection
    /// closed, signal handler).
    ///
    /// # Errors
    ///
    /// Returns error if the node task panicked or returned an error.
    pub async fn wait(self) -> anyhow::Result<()> {
        match self.node_task.await {
            Ok(result) => result,
            Err(e) if e.is_cancelled() => Ok(()),
            Err(e) => Err(anyhow::anyhow!("node task panicked: {e}")),
        }
    }
}
