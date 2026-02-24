use std::sync::Arc;

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};

use super::{Shared, tcp_stream::TcpStream};
use crate::error::Error;

/// A TCP listener for a specific port backed by the smoltcp stack.
///
/// Uses a simple "seen list" approach - iterates all sockets in the stack,
/// returns ESTABLISHED ones that haven't been returned before.
pub struct TcpListener<D: Device + Send + 'static> {
    shared: Arc<Shared<D>>,
    port: u16,
    /// Handles we've already returned - don't return again
    seen: std::collections::HashSet<SocketHandle>,
}

impl<D: Device + Send + 'static> TcpListener<D> {
    /// Creates a new listener for the given port.
    pub(crate) fn new(shared: Arc<Shared<D>>, port: u16, _backlog_size: usize) -> Self {
        Self {
            shared,
            port,
            seen: std::collections::HashSet::new(),
        }
    }

    /// Accept the next incoming TCP connection.
    ///
    /// Waits until a connection is available.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener cannot poll for connections.
    pub async fn accept(&mut self) -> Result<TcpStream<D>, Error> {
        loop {
            if let Some(stream) = self.poll_accept()? {
                return Ok(stream);
            }
            self.shared.notify.notified().await;
        }
    }

    /// Poll for a new incoming TCP connection without waiting.
    ///
    /// Returns `Ok(None)` if no connection is ready yet.
    ///
    /// # Errors
    ///
    /// Currently always returns `Ok`. Reserved for future error conditions.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (another thread panicked while holding the lock).
    pub fn poll_accept(&mut self) -> Result<Option<TcpStream<D>>, Error> {
        let inner = self.shared.inner.lock();

        // Clean up the seen set by collecting active TCP handles.
        // This makes the prune O(M) where M is the number of active sockets.
        let active_handles: std::collections::HashSet<_> = inner
            .sockets()
            .iter()
            .filter_map(|(handle, socket)| {
                let smoltcp::socket::Socket::Tcp(tcp) = socket else {
                    return None;
                };
                if matches!(tcp.state(), tcp::State::Closed | tcp::State::TimeWait) {
                    None
                } else {
                    Some(handle)
                }
            })
            .collect();
        self.seen.retain(|h| active_handles.contains(h));

        // Find an ESTABLISHED socket for this port that hasn't been returned yet.
        for (handle, socket) in inner.sockets().iter() {
            let smoltcp::socket::Socket::Tcp(tcp_socket) = socket else {
                continue;
            };

            if tcp_socket.listen_endpoint().port != self.port {
                continue;
            }

            if self.seen.contains(&handle) {
                continue;
            }

            // Accept if established (or about to be)
            if tcp_socket.is_active() && tcp_socket.may_send() {
                tracing::debug!(port = self.port, ?handle, state = ?tcp_socket.state(), "Accepting connection");
                self.seen.insert(handle);
                return Ok(Some(TcpStream::new(Arc::clone(&self.shared), handle)));
            }
        }

        Ok(None)
    }

    /// Returns the port this listener is bound to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl<D: Device + Send + 'static> Drop for TcpListener<D> {
    fn drop(&mut self) {
        // Nothing to clean up - sockets are managed by JIT
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use smoltcp::wire::{IpCidr, TcpSeqNumber};
    use tokio::time::Instant;

    use super::{
        super::{Netstack, test_helpers::*},
        TcpListener,
    };
    use crate::{config::StackConfig, inner::device::VecDevice};

    fn make_stack_with_listener(port: u16) -> (Netstack<VecDevice>, TcpListener<VecDevice>) {
        let config = StackConfig {
            ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
            ..test_config()
        };
        let device = VecDevice::new(1500);
        let stack = Netstack::new(device, config);

        // Create an explicit LISTEN socket on the port
        {
            let mut inner = stack.shared.inner.lock();
            inner.tcp_listen(port).expect("tcp_listen");
        }

        let listener = stack.tcp_listen(port, 128);
        (stack, listener)
    }

    #[tokio::test]
    async fn test_accept_single_connection() {
        let (stack, mut listener) = make_stack_with_listener(8080);
        let hs = complete_handshake(&stack, 8080).await;

        let stream = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("timeout")
            .expect("accept");

        assert_eq!(stream.state(), smoltcp::socket::tcp::State::Established);
        let remote = stream.remote_endpoint().expect("remote_endpoint");
        assert_eq!(remote.port, CLIENT_SRC_PORT);
        let _ = hs;
    }

    #[tokio::test]
    async fn test_accept_blocks_until_connection() {
        let (stack, mut listener) = make_stack_with_listener(8080);

        // Spawn accept — should block
        let accept_handle = tokio::spawn(async move { listener.accept().await });

        // Give it some time to block
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!accept_handle.is_finished(), "accept should be blocking");

        // Now inject SYN to unblock
        complete_handshake(&stack, 8080).await;

        let result = tokio::time::timeout(Duration::from_secs(2), accept_handle)
            .await
            .expect("timeout")
            .expect("join")
            .expect("accept");

        assert_eq!(result.state(), smoltcp::socket::tcp::State::Established);
    }

    #[tokio::test]
    async fn test_poll_accept_none_when_empty() {
        let (_stack, mut listener) = make_stack_with_listener(8080);
        let result = listener.poll_accept().expect("poll_accept");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_accept_multiple_same_port() {
        let (stack, mut listener) = make_stack_with_listener(8080);

        // Complete 3 handshakes with different source ports
        for i in 0..3u16 {
            let src_port = 20000 + i;
            // Each connection needs its own LISTEN socket in smoltcp
            {
                let mut inner = stack.shared.inner.lock();
                inner.tcp_listen(8080).expect("tcp_listen");
            }
            complete_handshake_from(
                &stack,
                src_port,
                8080,
                TcpSeqNumber(5000 + i32::from(i) * 100),
            )
            .await;
        }

        // Accept all 3
        let mut streams = Vec::new();
        let start = Instant::now();
        while streams.len() < 3 {
            if let Ok(Some(s)) = listener.poll_accept() {
                streams.push(s);
            }
            assert!(
                start.elapsed() <= Duration::from_secs(2),
                "Timeout: only accepted {}/3 connections",
                streams.len()
            );
            tokio::task::yield_now().await;
        }

        assert_eq!(streams.len(), 3);
        // Verify they have distinct remote ports
        let ports: std::collections::HashSet<_> = streams
            .iter()
            .filter_map(|s| s.remote_endpoint().map(|ep| ep.port))
            .collect();
        assert_eq!(ports.len(), 3, "expected 3 distinct remote ports");
    }

    #[tokio::test]
    async fn test_accept_ignores_wrong_port() {
        let (stack, mut listener) = make_stack_with_listener(8080);

        // Handshake on port 9090 (different from listener's 8080)
        {
            let mut inner = stack.shared.inner.lock();
            inner.tcp_listen(9090).expect("tcp_listen");
        }
        complete_handshake(&stack, 9090).await;

        // Listener on 8080 should see nothing
        let result = listener.poll_accept().expect("poll_accept");
        assert!(
            result.is_none(),
            "listener on 8080 should not see connection on 9090"
        );
    }

    #[tokio::test]
    async fn test_seen_prunes_closed() {
        let (stack, mut listener) = make_stack_with_listener(8080);
        complete_handshake(&stack, 8080).await;

        // Accept first connection
        let start = Instant::now();
        let stream = loop {
            if let Ok(Some(s)) = listener.poll_accept() {
                break s;
            }
            assert!(
                start.elapsed() <= Duration::from_secs(2),
                "Timeout waiting for first accept"
            );
            tokio::task::yield_now().await;
        };

        // Drop stream → socket gets closed
        drop(stream);

        // Prune closed sockets
        {
            let mut inner = stack.shared.inner.lock();
            let now = inner.now();
            inner.poll(now);
            inner.prune_closed_tcp_sockets();
        }

        // New connection on same port, different src_port
        {
            let mut inner = stack.shared.inner.lock();
            inner.tcp_listen(8080).expect("tcp_listen");
        }
        complete_handshake_from(&stack, 30000, 8080, TcpSeqNumber(9000)).await;

        // Should be able to accept the new connection
        let start2 = Instant::now();
        let stream2 = loop {
            if let Ok(Some(s)) = listener.poll_accept() {
                break s;
            }
            assert!(
                start2.elapsed() <= Duration::from_secs(2),
                "Timeout waiting for second accept"
            );
            tokio::task::yield_now().await;
        };
        assert_eq!(stream2.remote_endpoint().unwrap().port, 30000);
    }

    #[tokio::test]
    async fn test_port_accessor() {
        let (_stack, listener) = make_stack_with_listener(8080);
        assert_eq!(listener.port(), 8080);
    }
}
