//! Shared transport infrastructure for all node modes.
//!
//! Consolidates DNS resolution, retry-connect loops, and channel bridging
//! patterns that were duplicated across entry, exit, and relay.

use std::{net::SocketAddr, str::FromStr, time::Duration};

use tokio::sync::broadcast;

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

/// Retry a connection attempt with exponential backoff.
///
/// Calls `create_and_connect` in a loop. On success, returns the result.
/// On dead errors (TLS/cert failures), returns immediately.
/// On transient errors, retries with exponential backoff up to [`MAX_RETRY_DELAY`].
pub(crate) async fn connect_with_retry<T, E, F, Fut>(create_and_connect: F) -> Result<T, NodeError>
where
    E: std::error::Error + Send + Sync + 'static,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        match create_and_connect().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let err = NodeError::Transport(Box::new(e));
                if !err.is_retryable() {
                    tracing::error!("Connection failed (not retrying): {err}");
                    return Err(err);
                }
                tracing::warn!("Connection failed: {err}, retrying in {retry_delay:?}...");
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
            }
        }
    }
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

    loop {
        match create_and_connect().await {
            Ok(result) => {
                retry_delay = INITIAL_RETRY_DELAY;
                run_session(result).await?;
                tracing::warn!("Connection dropped, reconnecting in {reconnect_delay:?}...");
                tokio::time::sleep(reconnect_delay).await;
            }
            Err(e) => {
                let err = NodeError::Transport(Box::new(e));
                if !err.is_retryable() {
                    tracing::error!("Connection failed (not retrying): {err}");
                    return Err(err);
                }
                tracing::warn!("Connection failed: {err}, retrying in {retry_delay:?}...");
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

/// Bridge a peer connection's channels to source broadcast channels.
///
/// Forwards instructions from the peer to the source, and responses from the
/// source back to the peer. Holds the control channel sender alive for the
/// lifetime of the bridged connection.
///
/// This replaces the identical `bridge_downstream` (relay) and `bridge_peer`
/// (exit relay capability) functions.
pub(crate) fn bridge_channels<T: wallhack_core::transport::Transport>(
    accept_result: wallhack_core::server::server::AcceptResult<T>,
    source_instr: &broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: &broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) {
    tracing::debug!("Bridging peer connection: {}", accept_result.peer_addr());

    let ((peer_instr, peer_resp), control_tx) = accept_result.channels();

    // Forward peer instructions to source (also holds control_tx to keep control stream alive)
    let source_instr_clone = source_instr.clone();
    let mut peer_instr_rx = peer_instr.subscribe();
    tokio::spawn(async move {
        let _keep_alive = control_tx;
        loop {
            match peer_instr_rx.recv().await {
                Ok(instr) => {
                    if source_instr_clone.send(instr).is_err() {
                        tracing::warn!("Source instruction channel closed");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Lagged {n} instructions");
                }
            }
        }
    });

    // Forward source responses to peer
    let mut source_resp_rx = source_resp.subscribe();
    let peer_resp_clone = peer_resp.clone();
    tokio::spawn(async move {
        loop {
            match source_resp_rx.recv().await {
                Ok(resp) => {
                    if peer_resp_clone.send(resp).is_err() {
                        tracing::warn!("Peer response channel closed");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Lagged {n} responses");
                }
            }
        }
    });
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
