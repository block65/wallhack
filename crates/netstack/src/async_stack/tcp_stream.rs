use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::Shared;

/// An async TCP stream backed by the smoltcp stack.
///
/// Implements [`AsyncRead`] and [`AsyncWrite`]. When the stream is dropped the
/// underlying socket is aborted and the poll loop is notified.
pub struct TcpStream<D: Device + Send + 'static> {
    shared: Arc<Shared<D>>,
    handle: SocketHandle,
}

impl<D: Device + Send + 'static> TcpStream<D> {
    pub(crate) fn new(shared: Arc<Shared<D>>, handle: SocketHandle) -> Self {
        Self { shared, handle }
    }

    /// Returns the socket handle.
    #[must_use]
    pub fn handle(&self) -> SocketHandle {
        self.handle
    }

    /// Returns the current TCP state, or Closed if socket was pruned.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    #[must_use]
    pub fn state(&self) -> tcp::State {
        let inner = self.shared.inner.lock();
        inner
            .sockets()
            .iter()
            .find(|(h, _)| *h == self.handle)
            .and_then(|(_, s)| match s {
                smoltcp::socket::Socket::Tcp(tcp) => Some(tcp.state()),
                _ => None,
            })
            .unwrap_or(tcp::State::Closed)
    }

    /// Returns the remote endpoint for this stream, if connected.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    #[must_use]
    pub fn remote_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
        let inner = self.shared.inner.lock();
        inner
            .sockets()
            .iter()
            .find(|(h, _)| *h == self.handle)
            .and_then(|(_, s)| match s {
                smoltcp::socket::Socket::Tcp(tcp) => tcp.remote_endpoint(),
                _ => None,
            })
    }

    /// Returns the local endpoint for this stream, if connected.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    #[must_use]
    pub fn local_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
        let inner = self.shared.inner.lock();
        inner
            .sockets()
            .iter()
            .find(|(h, _)| *h == self.handle)
            .and_then(|(_, s)| match s {
                smoltcp::socket::Socket::Tcp(tcp) => tcp.local_endpoint(),
                _ => None,
            })
    }
}

impl<D: Device + Send + 'static> AsyncRead for TcpStream<D> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut inner = self.shared.inner.lock();

        let Some((_, smoltcp::socket::Socket::Tcp(socket))) = inner
            .sockets_mut()
            .iter_mut()
            .find(|(h, _)| *h == self.handle)
        else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "socket was closed or of wrong type",
            )));
        };

        socket.register_recv_waker(cx.waker());

        if !socket.may_recv() {
            return Poll::Ready(Ok(()));
        }

        match socket.recv_slice(buf.initialize_unfilled()) {
            Ok(0) => Poll::Pending,
            Ok(n) => {
                tracing::trace!(bytes = n, "TcpStream recv");
                buf.advance(n);
                drop(inner);
                self.shared.notify.notify_one();
                Poll::Ready(Ok(()))
            }
            Err(tcp::RecvError::Finished) => Poll::Ready(Ok(())),
            Err(tcp::RecvError::InvalidState) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "recv: invalid state",
            ))),
        }
    }
}

impl<D: Device + Send + 'static> AsyncWrite for TcpStream<D> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner = self.shared.inner.lock();

        let Some((_, smoltcp::socket::Socket::Tcp(socket))) = inner
            .sockets_mut()
            .iter_mut()
            .find(|(h, _)| *h == self.handle)
        else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "socket was closed or of wrong type",
            )));
        };

        socket.register_send_waker(cx.waker());

        if !socket.may_send() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "send: socket not writable",
            )));
        }

        match socket.send_slice(buf) {
            Ok(0) => Poll::Pending,
            Ok(n) => {
                tracing::trace!(bytes = n, "TcpStream send");
                drop(inner);
                self.shared.notify.notify_one();
                Poll::Ready(Ok(n))
            }
            Err(tcp::SendError::InvalidState) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "send: invalid state",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared.notify.notify_one();
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.shared.inner.lock();

        if let Some((_, smoltcp::socket::Socket::Tcp(socket))) = inner
            .sockets_mut()
            .iter_mut()
            .find(|(h, _)| *h == self.handle)
        {
            socket.close();
        }

        drop(inner);
        self.shared.notify.notify_one();
        Poll::Ready(Ok(()))
    }
}

impl<D: Device + Send + 'static> Drop for TcpStream<D> {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock();

        if let Some((_, smoltcp::socket::Socket::Tcp(socket))) = inner
            .sockets_mut()
            .iter_mut()
            .find(|(h, _)| *h == self.handle)
        {
            // Close the socket gracefully (sends FIN, not RST).
            socket.close();
            tracing::debug!(handle = ?self.handle, "TcpStream dropped, socket closed");
        } else {
            tracing::trace!(handle = ?self.handle, "TcpStream dropped but socket already gone");
        }

        drop(inner);
        self.shared.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use smoltcp::wire::{IpCidr, Ipv4Packet, TcpPacket};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::Instant,
    };

    use super::{
        super::{Netstack, test_helpers::*},
        TcpStream,
    };
    use crate::{config::StackConfig, inner::device::VecDevice};

    /// Create a Netstack with JIT TCP enabled and complete a handshake,
    /// returning (stack, stream) ready for read/write tests.
    async fn setup_connected_stream() -> (Netstack<VecDevice>, TcpStream<VecDevice>, HandshakeResult)
    {
        let config = StackConfig {
            ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
            ..test_config()
        };
        let device = VecDevice::new(1500);
        let mut stack = Netstack::new(device, config);
        let mut listener = stack.tcp_listen_any().expect("tcp_listen_any");

        let port = 8080;
        let hs = complete_handshake(&stack, port).await;

        // Wait for stream to become accepted
        let start = Instant::now();
        let stream = loop {
            if let Ok(Some(s)) = listener.poll_accept() {
                break s;
            }
            assert!(
                start.elapsed() <= Duration::from_secs(2),
                "Timeout waiting for accept"
            );
            tokio::task::yield_now().await;
        };

        (stack, stream, hs)
    }

    // ============================================================================
    // Read tests
    // ============================================================================

    #[tokio::test]
    async fn test_read_data() {
        let (stack, mut stream, hs) = setup_connected_stream().await;

        let payload = b"hello world";
        {
            let mut inner = stack.shared.inner.lock();
            inner.device_mut().inject(create_data_packet(
                8080,
                hs.client_next_seq,
                hs.server_next_seq,
                payload,
            ));
        }
        stack.wake();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("timeout")
            .expect("read");

        assert_eq!(&buf[..n], payload);
    }

    #[tokio::test]
    async fn test_read_returns_zero_on_fin() {
        let (stack, mut stream, hs) = setup_connected_stream().await;

        {
            let mut inner = stack.shared.inner.lock();
            inner.device_mut().inject(create_fin_packet(
                8080,
                hs.client_next_seq,
                hs.server_next_seq,
            ));
        }
        stack.wake();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("timeout")
            .expect("read");

        assert_eq!(n, 0, "FIN should cause read to return 0 (EOF)");
    }

    #[tokio::test]
    async fn test_read_not_connected_after_pruned() {
        let (stack, mut stream, _hs) = setup_connected_stream().await;

        {
            let mut inner = stack.shared.inner.lock();
            inner.remove_socket(stream.handle());
        }

        let mut buf = [0u8; 64];
        let result = stream.read(&mut buf).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotConnected);
    }

    #[tokio::test]
    async fn test_read_multiple_chunks() {
        let (stack, mut stream, hs) = setup_connected_stream().await;

        let chunk1 = b"first";
        let chunk2 = b"second";

        // Inject first data packet
        {
            let mut inner = stack.shared.inner.lock();
            inner.device_mut().inject(create_data_packet(
                8080,
                hs.client_next_seq,
                hs.server_next_seq,
                chunk1,
            ));
        }
        stack.wake();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut buf = [0u8; 64];
        let n1 = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("timeout")
            .expect("read");
        assert_eq!(&buf[..n1], chunk1);

        // Drain egress (ACK for first chunk) so the stack can process the second
        {
            let mut inner = stack.shared.inner.lock();
            inner.device_mut().drain_egress();
        }

        // Inject second data packet (seq advances by chunk1 length)
        let next_seq = hs.client_next_seq + chunk1.len();
        {
            let mut inner = stack.shared.inner.lock();
            inner.device_mut().inject(create_data_packet(
                8080,
                next_seq,
                hs.server_next_seq,
                chunk2,
            ));
        }
        stack.wake();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let n2 = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("timeout")
            .expect("read");
        assert_eq!(&buf[..n2], chunk2);
    }

    // ============================================================================
    // Write tests
    // ============================================================================

    #[tokio::test]
    async fn test_write_data() {
        let (stack, mut stream, _hs) = setup_connected_stream().await;

        stream.write_all(b"hello").await.expect("write_all");

        // Drive the stack to emit the data
        {
            let mut inner = stack.shared.inner.lock();
            let now = inner.now();
            inner.poll(now);
            let egress = inner.device_mut().drain_egress();
            let found = egress.iter().any(|pkt| {
                if let Ok(ip_pkt) = Ipv4Packet::new_checked(pkt.as_slice())
                    && let Ok(tcp_pkt) = TcpPacket::new_checked(ip_pkt.payload())
                {
                    return tcp_pkt.payload().windows(5).any(|w| w == b"hello");
                }
                false
            });
            assert!(found, "expected 'hello' payload in egress");
        }
    }

    #[tokio::test]
    async fn test_write_broken_pipe_after_shutdown() {
        let (_stack, mut stream, _hs) = setup_connected_stream().await;

        stream.shutdown().await.expect("shutdown");

        let result = stream.write(b"data").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn test_write_not_connected_after_pruned() {
        let (stack, mut stream, _hs) = setup_connected_stream().await;

        {
            let mut inner = stack.shared.inner.lock();
            inner.remove_socket(stream.handle());
        }

        let result = stream.write(b"data").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotConnected);
    }

    // ============================================================================
    // Flush / Shutdown tests
    // ============================================================================

    #[tokio::test]
    async fn test_flush_always_ready() {
        let (_stack, mut stream, _hs) = setup_connected_stream().await;
        stream.flush().await.expect("flush should always succeed");
    }

    #[tokio::test]
    async fn test_shutdown_sends_fin() {
        let (stack, mut stream, _hs) = setup_connected_stream().await;

        stream.shutdown().await.expect("shutdown");

        {
            let mut inner = stack.shared.inner.lock();
            let now = inner.now();
            inner.poll(now);
            let egress = inner.device_mut().drain_egress();
            let has_fin = egress.iter().any(|pkt| {
                if let Ok(ip_pkt) = Ipv4Packet::new_checked(pkt.as_slice())
                    && let Ok(tcp_pkt) = TcpPacket::new_checked(ip_pkt.payload())
                {
                    return tcp_pkt.fin();
                }
                false
            });
            assert!(has_fin, "expected FIN in egress after shutdown");
        }
    }

    // ============================================================================
    // Drop tests
    // ============================================================================

    #[tokio::test]
    async fn test_drop_closes_socket() {
        let (stack, stream, _hs) = setup_connected_stream().await;
        let handle = stream.handle();

        assert_eq!(stream.state(), smoltcp::socket::tcp::State::Established);
        drop(stream);

        let inner = stack.shared.inner.lock();
        if let Some((_, smoltcp::socket::Socket::Tcp(tcp))) =
            inner.sockets().iter().find(|(h, _)| *h == handle)
        {
            assert_ne!(tcp.state(), smoltcp::socket::tcp::State::Established);
        }
    }

    #[tokio::test]
    async fn test_drop_when_socket_gone() {
        let (stack, stream, _hs) = setup_connected_stream().await;

        {
            let mut inner = stack.shared.inner.lock();
            inner.remove_socket(stream.handle());
        }

        // Drop should not panic
        drop(stream);
    }

    // ============================================================================
    // Accessor tests
    // ============================================================================

    #[tokio::test]
    async fn test_handle_accessor() {
        let (_stack, stream, _hs) = setup_connected_stream().await;
        let _handle = stream.handle();
    }

    #[tokio::test]
    async fn test_state_accessor() {
        let (_stack, mut stream, _hs) = setup_connected_stream().await;

        assert_eq!(stream.state(), smoltcp::socket::tcp::State::Established);

        stream.shutdown().await.expect("shutdown");
        let state = stream.state();
        // After shutdown, state should be FinWait1 or later
        assert_ne!(state, smoltcp::socket::tcp::State::Established);
    }

    #[tokio::test]
    async fn test_state_closed_when_pruned() {
        let (stack, stream, _hs) = setup_connected_stream().await;

        {
            let mut inner = stack.shared.inner.lock();
            inner.remove_socket(stream.handle());
        }

        assert_eq!(stream.state(), smoltcp::socket::tcp::State::Closed);
    }

    #[tokio::test]
    async fn test_remote_endpoint() {
        let (_stack, stream, _hs) = setup_connected_stream().await;

        let remote = stream.remote_endpoint();
        assert!(remote.is_some());
        let ep = remote.unwrap();
        assert_eq!(ep.addr, CLIENT_IP.into());
        assert_eq!(ep.port, CLIENT_SRC_PORT);
    }

    #[tokio::test]
    async fn test_local_endpoint() {
        let (_stack, stream, _hs) = setup_connected_stream().await;

        let local = stream.local_endpoint();
        assert!(local.is_some());
        let ep = local.unwrap();
        assert_eq!(ep.addr, STACK_IP.into());
        assert_eq!(ep.port, 8080);
    }
}
