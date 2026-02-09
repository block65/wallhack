use std::{collections::VecDeque, sync::Arc};

use netstack::{
	async_stack::{Netstack, ReadinessFn},
	config::StackConfig,
};
use smoltcp::wire::{IpCidr, Ipv4Address, Ipv6Address};
use tokio::io::unix::AsyncFd;
use tun::{AbstractDevice, Configuration, Device};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("tun error: {0}")]
	Tun(#[from] tun::Error),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

pub struct TunActor {
	pub name: String,
	stack: Netstack<SmoltcpTunDevice>,
	async_device: Arc<AsyncFd<Device>>,
}

impl TunActor {
	#[must_use]
	pub fn random_iface_name() -> String {
		random_iface_name()
	}

	pub fn new(name: Option<String>) -> Result<Self, Error> {
		let tun_name = name.unwrap_or_else(random_iface_name);
		let mut config = Configuration::default();
		config.tun_name(tun_name.clone());
		config.up();

		let device = Device::new(&config)?;
		device.set_nonblock()?;
		let name = device.tun_name()?;
		let mtu = device.mtu().unwrap_or(1500) as usize;

		// Wrap the TUN device in AsyncFd for epoll-based readiness notification.
		let async_device = Arc::new(AsyncFd::new(device)?);
		let smoltcp_dev = SmoltcpTunDevice::new(Arc::clone(&async_device), mtu);

		// For AnyIP to work, smoltcp needs:
		// 1. An IP address on the interface (we use 0.0.0.0/0 as a wildcard)
		// 2. any_ip enabled
		let stack_config = StackConfig {
			ip_addrs: vec![
				IpCidr::new(Ipv4Address::UNSPECIFIED.into(), 0),
				IpCidr::new(Ipv6Address::UNSPECIFIED.into(), 0),
			],
			mtu,
			any_ip: true,
			..StackConfig::default()
		};

		let stack = Netstack::new(smoltcp_dev, stack_config);

		Ok(Self {
			name,
			stack,
			async_device,
		})
	}

	pub fn stack(&mut self) -> &mut Netstack<SmoltcpTunDevice> {
		&mut self.stack
	}

	/// Consume the actor, returning the netstack with an epoll-based readiness
	/// callback already configured. This replaces the 1ms sleep poll with
	/// proper fd readiness notification.
	#[must_use]
	pub fn into_stack(mut self) -> Netstack<SmoltcpTunDevice> {
		let fd = self.async_device;
		let readiness_fn: ReadinessFn = Arc::new(move || {
			let fd = Arc::clone(&fd);
			Box::pin(async move {
				if let Ok(mut guard) = fd.readable().await {
					guard.clear_ready();
				}
			})
		});
		self.stack.set_readable_fn(readiness_fn);
		self.stack
	}
}

fn random_iface_name() -> String {
	use rand::Rng;

	const IFACE_NAME_CHARSET: &[u8] = b"dfgjpqstz2346789";
	const IFACE_NAME_SUFFIX_LEN: usize = 4;

	let mut rng = rand::rng();
	let index = rng.random_range(0..9);
	let rand: String = (0..IFACE_NAME_SUFFIX_LEN)
		.map(|_| {
			let idx = rng.random_range(0..IFACE_NAME_CHARSET.len());
			IFACE_NAME_CHARSET[idx] as char
		})
		.collect();

	let prefix = std::env::var("CARGO_PKG_NAME")
		.unwrap_or_else(|_| "tun".to_string())
		.chars()
		.filter(char::is_ascii_alphanumeric)
		.collect::<String>();

	format!("{prefix}{index}{rand}")
}

pub struct SmoltcpTunDevice {
	inner: Arc<AsyncFd<Device>>,
	mtu: usize,
	pending: VecDeque<Vec<u8>>,
}

impl SmoltcpTunDevice {
	fn new(inner: Arc<AsyncFd<Device>>, mtu: usize) -> Self {
		Self {
			inner,
			mtu,
			pending: VecDeque::new(),
		}
	}

	fn read_packet(&self, mtu: usize) -> std::io::Result<Option<Vec<u8>>> {
		let mut buf = vec![0u8; mtu];
		match self.inner.get_ref().recv(&mut buf) {
			Ok(0) => Ok(None),
			Ok(n) => {
				buf.truncate(n);
				Ok(Some(buf))
			}
			Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
			Err(e) => {
				tracing::warn!("read_packet: error {e}");
				Err(e)
			}
		}
	}
}

impl netstack::inner::peek_device::PeekDevice for SmoltcpTunDevice {
	fn peek_ingress(&mut self) -> Option<&[u8]> {
		// Drain ALL available packets from the TUN device into pending.
		// This is critical for handling bursts of SYNs - we need to see
		// all of them BEFORE poll() processes them, so JIT can create
		// a LISTEN socket for each one.
		loop {
			match self.read_packet(self.mtu) {
				Ok(Some(packet)) => {
					self.pending.push_back(packet);
				}
				Ok(None) => break, // No more packets available
				Err(e) => {
					tracing::warn!("tun peek failed: {e}");
					break;
				}
			}
		}
		self.pending.front().map(std::vec::Vec::as_slice)
	}

	fn peek_all_ingress(&mut self) -> &VecDeque<Vec<u8>> {
		// First drain all available packets
		let _ = self.peek_ingress();
		// Return reference to pending packets
		&self.pending
	}
}

impl smoltcp::phy::Device for SmoltcpTunDevice {
	type RxToken<'a> = TunRxToken;
	type TxToken<'a> = TunTxToken;

	fn receive(
		&mut self,
		_timestamp: smoltcp::time::Instant,
	) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
		let buffer = match self.pending.pop_front() {
			Some(buffer) => buffer,
			None => match self.read_packet(self.mtu) {
				Ok(Some(packet)) => packet,
				Ok(None) => return None,
				Err(e) => {
					tracing::warn!("tun recv failed: {e}");
					return None;
				}
			},
		};

		Some((
			TunRxToken { buffer },
			TunTxToken {
				inner: Arc::clone(&self.inner),
			},
		))
	}

	fn transmit(&mut self, _timestamp: smoltcp::time::Instant) -> Option<Self::TxToken<'_>> {
		Some(TunTxToken {
			inner: Arc::clone(&self.inner),
		})
	}

	fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
		let mut caps = smoltcp::phy::DeviceCapabilities::default();
		caps.max_transmission_unit = self.mtu;
		caps.medium = smoltcp::phy::Medium::Ip;
		// TUN devices don't have hardware checksum offload - smoltcp must compute checksums
		caps.checksum.ipv4 = smoltcp::phy::Checksum::Tx;
		caps.checksum.tcp = smoltcp::phy::Checksum::Tx;
		caps.checksum.udp = smoltcp::phy::Checksum::Tx;
		caps.checksum.icmpv4 = smoltcp::phy::Checksum::Tx;
		caps.checksum.icmpv6 = smoltcp::phy::Checksum::Tx;
		caps
	}
}

pub struct TunRxToken {
	buffer: Vec<u8>,
}

impl smoltcp::phy::RxToken for TunRxToken {
	fn consume<R, F>(self, f: F) -> R
	where
		F: FnOnce(&[u8]) -> R,
	{
		f(&self.buffer)
	}
}

pub struct TunTxToken {
	inner: Arc<AsyncFd<Device>>,
}

impl smoltcp::phy::TxToken for TunTxToken {
	fn consume<R, F>(self, len: usize, f: F) -> R
	where
		F: FnOnce(&mut [u8]) -> R,
	{
		let mut buf = vec![0u8; len];
		let result = f(&mut buf);
		if let Err(e) = self.inner.get_ref().send(&buf)
			&& e.kind() != std::io::ErrorKind::WouldBlock
		{
			tracing::warn!("tun send failed: {e}");
		}
		result
	}
}
