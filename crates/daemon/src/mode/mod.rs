//! Node mode implementations.
//!
//! Each mode handles one operational role (entry, exit, relay). The unified
//! [`run`] dispatcher routes to the appropriate mode based on the CLI command.

pub(crate) mod entry;
pub(crate) mod exit;
pub(crate) mod relay;

use std::sync::Arc;

use wallhack_core::control::{metrics::Metrics, peers::Registry, routes::SharedRouteTable};

use crate::{NodeError, WallhackCli, cli::Command};

/// Shared resources available to all node modes.
pub(crate) struct NodeResources {
	pub metrics: Arc<Metrics>,
	pub peers: Arc<Registry>,
	pub routes: SharedRouteTable,
}

/// Dispatch to the appropriate node mode based on the CLI command.
///
/// # Errors
///
/// Returns error if the selected mode fails.
pub(crate) async fn run(
	global: &WallhackCli,
	command: &Command,
	resources: NodeResources,
) -> Result<(), NodeError> {
	match command {
		Command::Entry(cmd) => {
			entry::run(
				global,
				cmd,
				resources.metrics,
				resources.peers,
				resources.routes,
			)
			.await
		}
		Command::Exit(cmd) => exit::run(global, cmd, resources.metrics).await,
		Command::Relay(cmd) => relay::run(global, cmd, resources.metrics).await,
	}
}
