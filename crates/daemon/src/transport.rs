//! Shared transport infrastructure for all node modes.
//!
//! Consolidates DNS resolution, retry-connect loops, and channel bridging
//! patterns that were duplicated across entry, exit, and relay.

use std::{net::SocketAddr, str::FromStr, time::Duration};

use tokio::sync::mpsc;

use crate::NodeError;

/// Initial retry delay for connection attempts.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(50);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Resolve a hostname:port to a [`SocketAddr`], using an optional custom DNS server.
///
/// Tries parsing as an IP literal first, then falls back to DNS resolution.
pub(crate) async fn resolve_endpoint(
    addr: &str,
    dns_server: Option<&str>,
) -> Result<SocketAddr, NodeError> {
    let resolvable = crate::dns::ResolvableAddress::from_str(addr)
        .map_err(|e| NodeError::DnsResolution(Box::new(e)))?;
    let dns_server = dns_server
        .map(crate::dns::parse_str_to_addr)
        .transpose()
        .map_err(|e| NodeError::DnsResolution(Box::new(e)))?;

    let is_hostname = resolvable.hostname.parse::<std::net::IpAddr>().is_err();
    let resolved = crate::dns::resolve(resolvable, dns_server)
        .await
        .map_err(|e| NodeError::DnsResolution(Box::new(e)))?;

    if is_hostname {
        tracing::info!("Resolved {addr} as {resolved}");
    }

    Ok(resolved)
}

/// Persistent connect loop: connect, run a session, reconnect on drop.
///
/// Calls `create_and_connect` to establish a connection, then passes the
/// result to `run_session`. When the session ends, reconnects after a delay.
/// Non-retryable errors from `create_and_connect` are propagated.
pub(crate) async fn connect_loop<T, E, F, Fut, S, SFut>(
    create_and_connect: F,
    run_session: S,
    reconnect_delay: Duration,
) -> Result<(), NodeError>
where
    E: std::error::Error + Send + Sync + 'static,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    S: Fn(T) -> SFut,
    SFut: std::future::Future<Output = Result<(), NodeError>>,
{
    let mut retry_delay = INITIAL_RETRY_DELAY;
    let mut drop_delay = reconnect_delay;
    let mut last_error: Option<String> = None;
    let mut repeat_count: u32 = 0;

    loop {
        match create_and_connect().await {
            Ok(result) => {
                retry_delay = INITIAL_RETRY_DELAY;
                last_error = None;
                repeat_count = 0;
                let session_start = tokio::time::Instant::now();
                run_session(result).await?;
                // Reset backoff if the session was stable (lasted longer than
                // the current delay), otherwise keep increasing.
                if session_start.elapsed() > drop_delay {
                    drop_delay = reconnect_delay;
                }
                tracing::warn!("Connection dropped, reconnecting in {drop_delay:?}...");
                tokio::time::sleep(drop_delay).await;
                drop_delay = (drop_delay * 2).min(MAX_RETRY_DELAY);
            }
            Err(e) => {
                let err = NodeError::Transport(Box::new(e));
                if !err.is_retryable() {
                    tracing::error!("Connection failed (not retrying): {err}");
                    return Err(err);
                }
                let msg = err.to_string();
                if last_error.as_deref() == Some(&msg) {
                    repeat_count += 1;
                    tracing::warn!(
                        "Connection failed (repeated x{repeat_count}), retrying in {retry_delay:?}..."
                    );
                } else {
                    repeat_count = 1;
                    last_error = Some(msg);
                    tracing::warn!("Connection failed: {err}, retrying in {retry_delay:?}...");
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

/// Bridge a peer connection's channels to source mpsc channels.
///
/// Forwards instructions from the peer to the source (fan-in: N peers → 1 source).
/// Registers the peer's response sender with the fan-out task for the source.
/// Holds the control channel sender alive for the lifetime of the bridged connection.
///
/// `fanout_register_tx` is a channel to the relay's fan-out task: each new
/// peer sends its `responses_tx` there so the fan-out task can include it.
pub(crate) fn bridge_channels(
    peer_addr: &str,
    peer_instructions_rx: mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    peer_responses_tx: mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    source_instr_tx: mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: &mpsc::UnboundedSender<mpsc::Sender<wallhack_wire::data::ExitNodeResponse>>,
) {
    tracing::debug!("Bridging peer connection: {peer_addr}");

    // Register this peer's response sender with the fan-out task.
    if fanout_register_tx.send(peer_responses_tx).is_err() {
        tracing::warn!("Fan-out task closed, dropping peer {peer_addr}");
        return;
    }

    // Forward peer instructions to source (fan-in).
    // Also holds control_tx to keep the control stream alive.
    let mut instructions_rx = peer_instructions_rx;
    tokio::spawn(async move {
        let _keep_alive = control_tx;
        while let Some(instr) = instructions_rx.recv().await {
            if source_instr_tx.send(instr).await.is_err() {
                tracing::warn!("Source instruction channel closed");
                break;
            }
        }
    });
}

/// Spawn the relay fan-out task for a source connection.
///
/// The fan-out task reads responses from `source_resp_rx` and forwards each
/// to all currently-registered peer response senders. Returns a registration
/// channel: callers send a new `mpsc::Sender` for each peer that connects.
pub(crate) fn spawn_fanout_task(
    source_resp_rx: mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
) -> mpsc::UnboundedSender<mpsc::Sender<wallhack_wire::data::ExitNodeResponse>> {
    let (register_tx, mut register_rx) =
        mpsc::unbounded_channel::<mpsc::Sender<wallhack_wire::data::ExitNodeResponse>>();

    tokio::spawn(async move {
        let mut peers: Vec<mpsc::Sender<wallhack_wire::data::ExitNodeResponse>> = Vec::new();
        let mut source_resp_rx = source_resp_rx;

        loop {
            tokio::select! {
                // New peer registered
                Some(peer_tx) = register_rx.recv() => {
                    peers.push(peer_tx);
                }
                // Response from source
                result = source_resp_rx.recv() => {
                    let Some(resp) = result else {
                        tracing::debug!("Source response channel closed, fan-out task exiting");
                        break;
                    };
                    peers.retain(|tx| {
                        match tx.try_send(resp.clone()) {
                            Ok(()) => true,
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!("Fan-out: peer channel full, dropping response");
                                true
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => false,
                        }
                    });
                }
            }
        }
    });

    register_tx
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;

    #[tokio::test]
    async fn connect_loop_reconnects_after_session_ends() {
        let connect_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&connect_count);

        // connect_loop runs forever, so we timeout after it has reconnected
        // multiple times.
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            connect_loop(
                || {
                    let cc = Arc::clone(&cc);
                    async move {
                        cc.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, std::io::Error>(())
                    }
                },
                |()| async { Ok(()) },
                Duration::from_millis(1),
            ),
        )
        .await;

        // Should have reconnected multiple times within the timeout.
        assert!(
            connect_count.load(Ordering::SeqCst) >= 3,
            "expected at least 3 connections, got {}",
            connect_count.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn connect_loop_session_error_propagates() {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connect_loop(
                || async { Ok::<_, std::io::Error>(()) },
                |()| async { Err(NodeError::TransportUnavailable("test")) },
                Duration::from_millis(1),
            ),
        )
        .await;

        let err = result
            .expect("should not timeout")
            .expect_err("session error should propagate");
        assert!(
            matches!(err, NodeError::TransportUnavailable("test")),
            "expected TransportUnavailable, got {err:?}"
        );
    }
}
