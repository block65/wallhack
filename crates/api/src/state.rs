//! Shared state for the REST API.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;
use wallhack_core::node_api::NodeApi;

use super::auth::Auth;

/// Event types for SSE stream.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Event {
	PeerConnected {
		peer: String,
		addr: String,
	},
	PeerDisconnected {
		peer: String,
		reason: String,
	},
	PeerLatency {
		peer: String,
		latency_ms: f64,
	},
	Error {
		message: String,
	},
	StatsUpdate {
		bytes_in: u64,
		bytes_out: u64,
		active_connections: u64,
	},
}

/// Shared state for the API server.
#[derive(Clone)]
pub struct State {
	pub(super) node_api: Arc<dyn NodeApi>,
	pub(super) events_tx: broadcast::Sender<Event>,
	pub(super) auth: Auth,
}

impl State {
	/// Create API state with a `NodeApi` implementation and optional auth.
	#[must_use]
	pub fn new(node_api: Arc<dyn NodeApi>, auth: Auth) -> Self {
		let (events_tx, _) = broadcast::channel(256);
		Self {
			node_api,
			events_tx,
			auth,
		}
	}

	/// Emit an event to all SSE subscribers.
	pub fn emit(&self, event: Event) {
		let _ = self.events_tx.send(event);
	}
}
