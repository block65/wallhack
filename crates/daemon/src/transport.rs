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
const MAX_BACKOFF_DELAY: Duration = Duration::from_secs(30);

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
    let mut attempt: u32 = 0;

    loop {
        match create_and_connect().await {
            Ok(result) => {
                retry_delay = INITIAL_RETRY_DELAY;
                last_error = None;
                repeat_count = 0;
                attempt = 0;
                let session_start = tokio::time::Instant::now();
                run_session(result).await?;
                attempt += 1;
                // Reset backoff if the session was stable (lasted longer than
                // the current delay), otherwise keep increasing.
                if session_start.elapsed() > drop_delay {
                    drop_delay = reconnect_delay;
                }
                tracing::warn!("Session ended, reconnecting in {drop_delay:?} (attempt {attempt})");
                tokio::time::sleep(drop_delay).await;
                drop_delay = (drop_delay * 2).min(MAX_BACKOFF_DELAY);
            }
            Err(e) => {
                let err = NodeError::Transport(Box::new(e));
                if !err.is_retryable() {
                    tracing::error!("Connection failed (not retrying): {err}");
                    return Err(err);
                }
                attempt += 1;
                let msg = err.to_string();
                if last_error.as_deref() == Some(&msg) {
                    repeat_count += 1;
                    tracing::warn!(
                        "Reconnecting in {retry_delay:?} (attempt {attempt}, same error x{repeat_count})"
                    );
                } else {
                    repeat_count = 1;
                    last_error = Some(msg);
                    tracing::warn!("Reconnecting in {retry_delay:?} (attempt {attempt}): {err}");
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(MAX_BACKOFF_DELAY);
            }
        }
    }
}

/// Bridge a relay exit-peer connection's channels to source mpsc channels.
///
/// Registers the exit peer's instruction sender with the instruction fan-out
/// so it receives instructions from the entry. Spawns a task that reads
/// responses from the exit peer and forwards them to the entry via
/// `source_resp_tx` (fan-in: N exits → 1 entry). Holds `control_tx` alive
/// for the lifetime of the bridged connection.
///
/// `fanout_register_tx` is the registration channel for the instruction
/// fan-out task; sending a `Sender<EntryNodeInstruction>` enrolls the exit
/// peer to receive instructions forwarded from the entry.
pub(crate) fn relay_bridge_channels(
    peer_addr: &str,
    peer_instructions_tx: mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    peer_responses_rx: mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    source_resp_tx: mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: &mpsc::UnboundedSender<
        mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
) {
    tracing::debug!("Bridging relay exit-peer connection: {peer_addr}");

    // Enroll this exit peer in the instruction fan-out so it receives
    // instructions forwarded from the entry.
    if fanout_register_tx.send(peer_instructions_tx).is_err() {
        tracing::warn!("Instruction fan-out task closed, dropping exit peer {peer_addr}");
        return;
    }

    // Forward exit-peer responses to the entry (fan-in).
    // Also holds control_tx to keep the control stream alive.
    let mut responses_rx = peer_responses_rx;
    tokio::spawn(async move {
        let _keep_alive = control_tx;
        while let Some(resp) = responses_rx.recv().await {
            if source_resp_tx.send(resp).await.is_err() {
                tracing::warn!("Source response channel closed");
                break;
            }
        }
    });
}

/// Spawn the relay fan-out task for a source connection.
///
/// The fan-out task reads items from `source_rx` and forwards a clone of each
/// to all currently-registered peer senders. Returns a registration channel:
/// callers send a new `mpsc::Sender<T>` for each peer that connects.
pub(crate) fn spawn_fanout_task<T>(
    source_rx: mpsc::Receiver<T>,
) -> mpsc::UnboundedSender<mpsc::Sender<T>>
where
    T: Clone + Send + 'static,
{
    let (register_tx, mut register_rx) = mpsc::unbounded_channel::<mpsc::Sender<T>>();

    tokio::spawn(async move {
        let mut peers: Vec<mpsc::Sender<T>> = Vec::new();
        let mut source_rx = source_rx;

        loop {
            tokio::select! {
                // New peer registered
                Some(peer_tx) = register_rx.recv() => {
                    peers.push(peer_tx);
                }
                // Item from source
                result = source_rx.recv() => {
                    let Some(item) = result else {
                        tracing::debug!("Source channel closed, fan-out task exiting");
                        break;
                    };
                    peers.retain(|tx| {
                        match tx.try_send(item.clone()) {
                            Ok(()) => true,
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!("Fan-out: peer channel full, dropping item");
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
