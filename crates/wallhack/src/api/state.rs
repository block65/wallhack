//! Shared state for the REST API.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::control::{
	handler::{Handler, HandlerConfig},
	metrics::SharedMetrics,
	peers::SharedRegistry,
	routes::SharedRouteTable,
};

use super::{auth::Auth, node_api::NodeApi};

/// Event types for SSE stream.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Event {
	PeerConnected {
		peer_id: String,
		addr: String,
	},
	PeerDisconnected {
		peer_id: String,
		reason: String,
	},
	PeerLatency {
		peer_id: String,
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
	/// Create API state with handler and optional auth.
	#[must_use]
	pub fn new(
		handler_config: HandlerConfig,
		metrics: SharedMetrics,
		peers: SharedRegistry,
		routes: SharedRouteTable,
		auth: Auth,
	) -> Self {
		let (events_tx, _) = broadcast::channel(256);
		Self {
			node_api: Arc::new(Handler::new(handler_config, metrics, peers, routes)),
			events_tx,
			auth,
		}
	}

	/// Emit an event to all SSE subscribers.
	pub fn emit(&self, event: Event) {
		let _ = self.events_tx.send(event);
	}
}
