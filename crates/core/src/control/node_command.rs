//! Commands sent from the control API layer to the running mode task.
//!
//! [`NodeCommand`] is the message type used by [`super::handler::Handler`] to
//! request dynamic operations (connect, listen, disconnect) from the active
//! mode task. Replies are delivered via a one-shot channel embedded in each
//! variant.
//!
//! The mode task receives commands from an `mpsc::Receiver<NodeCommand>` and
//! the handler sends them via the paired `mpsc::Sender<NodeCommand>`. For
//! modes that do not support dynamic operations (entry, exit, relay), the
//! receiver end is simply dropped, which causes the handler's send to fail and
//! return a `NotSupported` error.

use std::net::SocketAddr;

use crate::node_api::{ConnectInfo, ListenInfo, NodeApiError};

/// Reply channel for a single command.
///
/// Uses a standard-library sync channel so the handler side (which may be
/// called from a synchronous context) can block waiting for the reply without
/// needing an async runtime handle.
pub type ReplySender<T> = std::sync::mpsc::SyncSender<Result<T, NodeApiError>>;
pub type ReplyReceiver<T> = std::sync::mpsc::Receiver<Result<T, NodeApiError>>;

/// Create a reply channel pair for a node command.
#[must_use]
pub fn reply_channel<T>() -> (ReplySender<T>, ReplyReceiver<T>) {
    std::sync::mpsc::sync_channel(1)
}

/// A command sent from the handler/API layer to the mode task.
#[derive(Debug)]
pub enum NodeCommand {
    /// Connect to a remote peer at the given address.
    Connect {
        /// Target address (host, host:port, etc.).
        addr: String,
        /// Channel for sending the result back to the caller.
        reply: ReplySender<ConnectInfo>,
    },
    /// Start listening for incoming peer connections.
    Listen {
        /// Address to bind.
        addr: SocketAddr,
        /// Channel for sending the result back to the caller.
        reply: ReplySender<ListenInfo>,
    },
    /// Disconnect from the currently connected peer.
    Disconnect {
        /// Channel for sending the result back to the caller.
        reply: ReplySender<()>,
    },
}
