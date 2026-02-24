use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use parking_lot::Mutex;
use smoltcp::{iface::SocketHandle, phy::Device};
use tokio::sync::Notify;

use crate::error::Error;

/// A UDP socket manager that binds per-port sockets on demand.
pub struct UdpSocketAny<D: Device + Send + 'static> {
    shared: Arc<super::Shared<D>>,
    notify: Arc<Notify>,
    ports: Arc<Mutex<HashSet<u16>>>,
    sockets: HashMap<u16, SocketHandle>,
}

impl<D: Device + Send + 'static> UdpSocketAny<D> {
    pub(crate) fn new(
        shared: Arc<super::Shared<D>>,
        notify: Arc<Notify>,
        ports: Arc<Mutex<HashSet<u16>>>,
    ) -> Self {
        Self {
            shared,
            notify,
            ports,
            sockets: HashMap::new(),
        }
    }

    /// Receive the next UDP packet from any bound port.
    ///
    /// Returns the data, metadata, and local port.
    ///
    /// # Errors
    ///
    /// Returns an error if receiving fails.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub async fn recv_from(
        &mut self,
        buf: &mut [u8],
    ) -> Result<(usize, smoltcp::socket::udp::UdpMetadata, u16), Error> {
        loop {
            self.refresh_sockets()?;

            {
                let mut inner = self.shared.inner.lock();
                for (port, handle) in &self.sockets {
                    let Some((_, smoltcp::socket::Socket::Udp(socket))) =
                        inner.sockets_mut().iter_mut().find(|(h, _)| *h == *handle)
                    else {
                        continue;
                    };

                    if socket.can_recv() {
                        let (size, meta) = socket.recv_slice(buf)?;
                        tracing::trace!(size, port, endpoint = %meta.endpoint, "UDP recv_from got packet");
                        return Ok((size, meta, *port));
                    }
                }
            }

            self.notify.notified().await;
        }
    }

    /// Send a UDP packet to the given remote endpoint using the specified local
    /// port.
    ///
    /// # Errors
    ///
    /// Returns an error if the send fails or the port is invalid.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn send_to(
        &mut self,
        port: u16,
        data: &[u8],
        meta: impl Into<smoltcp::socket::udp::UdpMetadata>,
    ) -> Result<(), Error> {
        self.refresh_sockets()?;
        let handle = *self.sockets.get(&port).ok_or(Error::InvalidPort { port })?;
        let mut inner = self.shared.inner.lock();

        let Some((_, smoltcp::socket::Socket::Udp(socket))) =
            inner.sockets_mut().iter_mut().find(|(h, _)| *h == handle)
        else {
            return Err(Error::InvalidHandle);
        };

        let meta_val = meta.into();
        tracing::debug!(port, data_len = data.len(), endpoint = %meta_val.endpoint, "UDP send_to enqueuing");
        socket.send_slice(data, meta_val)?;
        tracing::debug!(port, "UDP send_to enqueued successfully");
        drop(inner);
        self.shared.notify.notify_one();
        Ok(())
    }

    fn refresh_sockets(&mut self) -> Result<(), Error> {
        let ports_guard = self.ports.lock();
        if ports_guard.is_empty() {
            return Ok(());
        }

        let mut inner = self.shared.inner.lock();
        for port in ports_guard.iter() {
            let port = *port;
            if self.sockets.contains_key(&port) {
                continue;
            }
            inner.ensure_udp_listener(port)?;
            if let Some(handle) = find_udp_handle(&inner, port) {
                self.sockets.insert(port, handle);
            }
        }

        Ok(())
    }
}

fn find_udp_handle<D: Device>(
    inner: &crate::inner::InnerStack<D>,
    port: u16,
) -> Option<SocketHandle> {
    for (handle, socket) in inner.sockets().iter() {
        let smoltcp::socket::Socket::Udp(socket) = socket else {
            continue;
        };
        if socket.endpoint().port == port {
            return Some(handle);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use smoltcp::wire::IpCidr;

    use super::{
        super::{Netstack, test_helpers::*},
        UdpSocketAny,
    };
    use crate::{config::StackConfig, inner::device::VecDevice};

    fn make_udp_stack() -> (Netstack<VecDevice>, UdpSocketAny<VecDevice>) {
        let config = StackConfig {
            ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
            ..test_config()
        };
        let device = VecDevice::new(1500);
        let mut stack = Netstack::new(device, config);
        let socket = stack.udp_bind_any().expect("udp_bind_any");
        (stack, socket)
    }

    #[tokio::test]
    async fn test_udp_recv_single() {
        let (stack, mut socket) = make_udp_stack();

        let payload = b"hello udp";
        {
            let mut inner = stack.shared.inner.lock();
            inner
                .device_mut()
                .inject(create_udp_packet(50000, 5000, payload));
        }
        stack.wake();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut buf = [0u8; 128];
        let (size, meta, port) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf))
                .await
                .expect("timeout")
                .expect("recv_from");

        assert_eq!(&buf[..size], payload);
        assert_eq!(port, 5000);
        assert_eq!(meta.endpoint.port, 50000);
    }

    #[tokio::test]
    async fn test_udp_recv_blocks_until_data() {
        let (stack, mut socket) = make_udp_stack();

        let recv_handle = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            socket.recv_from(&mut buf).await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!recv_handle.is_finished(), "recv should be blocking");

        {
            let mut inner = stack.shared.inner.lock();
            inner
                .device_mut()
                .inject(create_udp_packet(50000, 5000, b"wake"));
        }
        stack.wake();

        let result = tokio::time::timeout(Duration::from_secs(2), recv_handle)
            .await
            .expect("timeout")
            .expect("join")
            .expect("recv_from");

        assert_eq!(result.0, 4); // "wake".len()
    }

    #[tokio::test]
    async fn test_udp_recv_multiple_ports() {
        let (stack, mut socket) = make_udp_stack();

        let ports = [5000u16, 5001, 5002];
        for &port in &ports {
            let mut inner = stack.shared.inner.lock();
            #[allow(clippy::cast_possible_truncation)]
            let tag = port as u8;
            inner
                .device_mut()
                .inject(create_udp_packet(50000, port, &[tag]));
        }
        stack.wake();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut received_ports = Vec::new();
        for _ in 0..3 {
            let mut buf = [0u8; 128];
            let (_size, _meta, port) =
                tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf))
                    .await
                    .expect("timeout")
                    .expect("recv_from");
            received_ports.push(port);
        }

        received_ports.sort_unstable();
        assert_eq!(received_ports, vec![5000, 5001, 5002]);
    }

    #[tokio::test]
    async fn test_udp_send_to() {
        let (stack, mut socket) = make_udp_stack();

        // Need a bound port first — inject a packet to trigger JIT binding
        {
            let mut inner = stack.shared.inner.lock();
            inner
                .device_mut()
                .inject(create_udp_packet(50000, 6000, b"init"));
        }
        stack.wake();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drain the init packet
        {
            let mut buf = [0u8; 128];
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buf)).await;
        }

        let meta = smoltcp::socket::udp::UdpMetadata::from(smoltcp::wire::IpEndpoint::new(
            CLIENT_IP.into(),
            50000,
        ));
        socket.send_to(6000, b"response", meta).expect("send_to");

        {
            let mut inner = stack.shared.inner.lock();
            let now = inner.now();
            inner.poll(now);
            let egress = inner.device_mut().drain_egress();
            assert!(!egress.is_empty(), "expected UDP response in egress");
        }
    }

    #[tokio::test]
    async fn test_udp_send_unknown_port_error() {
        let (_stack, mut socket) = make_udp_stack();

        let meta = smoltcp::socket::udp::UdpMetadata::from(smoltcp::wire::IpEndpoint::new(
            CLIENT_IP.into(),
            50000,
        ));
        let result = socket.send_to(9999, b"data", meta);
        assert!(result.is_err(), "send_to on unbound port should fail");
    }

    #[tokio::test]
    async fn test_udp_large_payload() {
        let (stack, mut socket) = make_udp_stack();

        let payload = vec![0xABu8; 1400]; // near MTU
        {
            let mut inner = stack.shared.inner.lock();
            inner
                .device_mut()
                .inject(create_udp_packet(50000, 7000, &payload));
        }
        stack.wake();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut buf = [0u8; 2048];
        let (size, _meta, port) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf))
                .await
                .expect("timeout")
                .expect("recv_from");

        assert_eq!(size, 1400);
        assert_eq!(port, 7000);
        assert!(buf[..size].iter().all(|&b| b == 0xAB));
    }
}
