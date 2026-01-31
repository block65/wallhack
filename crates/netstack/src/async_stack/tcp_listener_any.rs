use std::{
	collections::{HashMap, HashSet},
	sync::{Arc, Mutex},
};

use smoltcp::phy::Device;
use tokio::sync::Notify;

use super::tcp_listener::TcpListener;
use crate::error::Error;

/// A TCP listener that dynamically binds per-port listeners based on ingress
/// traffic.
///
/// This enables "listen any" behavior by creating listen sockets for
/// destination ports as they are observed. It relies on the poll loop to
/// install the listeners.
pub struct TcpListenerAny<D: Device + Send + 'static> {
	shared: Arc<super::Shared<D>>,
	notify: Arc<Notify>,
	ports: Arc<Mutex<HashSet<u16>>>,
	listeners: HashMap<u16, TcpListener<D>>,
	backlog: usize,
}

impl<D: Device + Send + 'static> TcpListenerAny<D> {
	pub(crate) fn new(
		shared: Arc<super::Shared<D>>,
		notify: Arc<Notify>,
		ports: Arc<Mutex<HashSet<u16>>>,
		backlog: usize,
	) -> Self {
		Self {
			shared,
			notify,
			ports,
			listeners: HashMap::new(),
			backlog: backlog.max(1),
		}
	}

	/// Accept the next incoming TCP connection.
	///
	/// # Errors
	///
	/// Returns an error if listener creation fails.
	///
	/// # Panics
	///
	/// Panics if the ports mutex is poisoned.
	pub async fn accept(&mut self) -> Result<super::tcp_stream::TcpStream<D>, Error> {
		loop {
			let ports = self.ports.lock().expect("ports mutex poisoned").clone();
			for port in &ports {
				if !self.listeners.contains_key(port) {
					tracing::trace!(port, "creating JIT TCP listener");
					let listener = TcpListener::new(Arc::clone(&self.shared), *port, self.backlog)?;
					self.listeners.insert(*port, listener);
				}
			}

			for (port, listener) in &mut self.listeners {
				if let Some(stream) = listener.poll_accept()? {
					tracing::trace!(port, "accepted TCP connection");
					return Ok(stream);
				}
			}

			self.notify.notified().await;
		}
	}
}
