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
					let socket: &mut smoltcp::socket::udp::Socket<'_> =
						inner.sockets_mut().get_mut(*handle);
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
		let socket: &mut smoltcp::socket::udp::Socket<'_> = inner.sockets_mut().get_mut(handle);
		let meta_val = meta.into();
		tracing::trace!(port, data_len = data.len(), endpoint = %meta_val.endpoint, "UDP send_to enqueuing");
		socket.send_slice(data, meta_val)?;
		tracing::trace!(port, "UDP send_to enqueued successfully");
		Ok(())
	}

	fn refresh_sockets(&mut self) -> Result<(), Error> {
		let ports = self.ports.lock().clone();
		if ports.is_empty() {
			return Ok(());
		}

		let mut inner = self.shared.inner.lock();
		for port in ports {
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
