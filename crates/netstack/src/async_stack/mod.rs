pub mod tcp_listener;
pub mod tcp_stream;

use std::sync::{Arc, Mutex};

use smoltcp::{phy::Device, time::Instant as SmolInstant};
use tokio::{sync::Notify, task::JoinHandle};

use crate::inner::InnerStack;

/// Shared state between the poll loop and async socket handles.
///
/// Uses [`std::sync::Mutex`] because the lock is never held across `.await`.
pub(crate) struct Shared<D: Device> {
	pub(crate) inner: Mutex<InnerStack<D>>,
	pub(crate) notify: Notify,
}

/// Asynchronous wrapper around [`InnerStack`].
///
/// `Netstack` spawns a background poll loop that drives the smoltcp state
/// machine. It provides [`TcpListener`](tcp_listener::TcpListener) and
/// [`TcpStream`](tcp_stream::TcpStream) types with standard async I/O traits.
///
/// # Examples
///
/// ```no_run
/// use netstack::async_stack::Netstack;
/// use netstack::inner::device::VecDevice;
/// use netstack::config::StackConfig;
/// use smoltcp::wire::{IpCidr, Ipv4Address};
///
/// # async fn example() {
/// let config = StackConfig {
///     ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
///     ..StackConfig::default()
/// };
/// let device = VecDevice::new(1500);
/// let stack = Netstack::new(device, config);
/// # }
/// ```
pub struct Netstack<D: Device + Send + 'static> {
	shared: Arc<Shared<D>>,
	poll_handle: JoinHandle<()>,
}

impl<D: Device + Send + 'static> Netstack<D> {
	/// Create a new async network stack and start the background poll loop.
	///
	/// # Panics
	///
	/// Panics if called outside a tokio runtime.
	pub fn new(device: D, config: crate::config::StackConfig) -> Self {
		let inner = InnerStack::new(device, config);
		let shared = Arc::new(Shared {
			inner: Mutex::new(inner),
			notify: Notify::new(),
		});

		let poll_handle = {
			let shared = Arc::clone(&shared);
			tokio::spawn(poll_loop(shared))
		};

		Self {
			shared,
			poll_handle,
		}
	}

	/// Create a TCP listener on the given port.
	///
	/// # Errors
	///
	/// Returns an error if the port is invalid or the listen socket cannot
	/// be created.
	pub fn tcp_listen(
		&self,
		port: u16,
		backlog: usize,
	) -> Result<tcp_listener::TcpListener<D>, crate::error::Error> {
		tcp_listener::TcpListener::new(Arc::clone(&self.shared), port, backlog)
	}

	/// Wake the poll loop to process pending work immediately.
	pub fn wake(&self) {
		self.shared.notify.notify_one();
	}
}

impl<D: Device + Send + 'static> Drop for Netstack<D> {
	fn drop(&mut self) {
		self.poll_handle.abort();
	}
}

/// Background poll loop that drives the smoltcp state machine.
///
/// Acquires the mutex, calls `InnerStack::poll()`, checks `poll_at()` for
/// the next deadline, then sleeps until either the deadline or a notification.
///
/// # Cancellation safety
///
/// This function is safe to cancel (abort) at any point — the only state
/// is behind the mutex, which is never held across an await.
async fn poll_loop<D: Device + Send + 'static>(shared: Arc<Shared<D>>) {
	loop {
		let delay = {
			let mut inner = shared.inner.lock().expect("poll loop mutex poisoned");
			let now = SmolInstant::from_millis(
				i64::try_from(
					std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.expect("system clock before epoch")
						.as_millis(),
				)
				.expect("timestamp overflow"),
			);
			inner.poll(now);
			inner.poll_at(now).map(|poll_at| {
				let diff = poll_at - now;
				tokio::time::Duration::from_millis(diff.total_millis())
			})
			// Lock is dropped here
		};

		match delay {
			Some(d) if d.is_zero() => {
				// Need to poll again immediately, but yield to let other tasks run
				tokio::task::yield_now().await;
			}
			Some(d) => {
				tokio::select! {
					() = tokio::time::sleep(d) => {}
					() = shared.notify.notified() => {}
				}
			}
			None => {
				// No timer pending; wait for external notification
				shared.notify.notified().await;
			}
		}
	}
}
