use std::collections::VecDeque;

use smoltcp::{
	phy::{self, Device, DeviceCapabilities, Medium},
	time::Instant,
};

/// A virtual network device backed by in-memory queues.
///
/// Packets injected via [`inject`](Self::inject) appear as received frames.
/// Packets transmitted by the stack are captured in the egress queue and
/// can be drained via [`drain_egress`](Self::drain_egress).
///
/// This device uses [`Medium::Ip`] (L3, no Ethernet framing), matching
/// TUN device semantics.
///
/// # Examples
///
/// ```
/// use netstack::inner::device::VecDevice;
///
/// let mut dev = VecDevice::new(1500);
/// dev.inject(vec![0u8; 40]);
/// assert_eq!(dev.ingress_len(), 1);
/// ```
#[derive(Debug)]
pub struct VecDevice {
	ingress: VecDeque<Vec<u8>>,
	egress: VecDeque<Vec<u8>>,
	mtu: usize,
}

impl VecDevice {
	/// Creates a new `VecDevice` with the given MTU.
	///
	/// # Examples
	///
	/// ```
	/// use netstack::inner::device::VecDevice;
	/// let dev = VecDevice::new(1500);
	/// ```
	#[must_use]
	pub fn new(mtu: usize) -> Self {
		Self {
			ingress: VecDeque::new(),
			egress: VecDeque::new(),
			mtu,
		}
	}

	/// Enqueue a raw IP packet for the stack to receive.
	pub fn inject(&mut self, packet: Vec<u8>) {
		self.ingress.push_back(packet);
	}

	/// Drain all transmitted packets from the egress queue.
	pub fn drain_egress(&mut self) -> Vec<Vec<u8>> {
		self.egress.drain(..).collect()
	}

	/// Returns the number of packets waiting in the ingress queue.
	#[must_use]
	pub fn ingress_len(&self) -> usize {
		self.ingress.len()
	}

	/// Returns the number of packets captured in the egress queue.
	#[must_use]
	pub fn egress_len(&self) -> usize {
		self.egress.len()
	}
}

impl Device for VecDevice {
	type RxToken<'a> = VecRxToken;
	type TxToken<'a> = VecTxToken<'a>;

	fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
		self.ingress.pop_front().map(|buffer| {
			let rx = VecRxToken { buffer };
			let tx = VecTxToken {
				queue: &mut self.egress,
			};
			(rx, tx)
		})
	}

	fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
		Some(VecTxToken {
			queue: &mut self.egress,
		})
	}

	fn capabilities(&self) -> DeviceCapabilities {
		let mut caps = DeviceCapabilities::default();
		caps.max_transmission_unit = self.mtu;
		caps.medium = Medium::Ip;
		caps
	}
}

/// Receive token for [`VecDevice`].
pub struct VecRxToken {
	buffer: Vec<u8>,
}

impl phy::RxToken for VecRxToken {
	fn consume<R, F>(self, f: F) -> R
	where
		F: FnOnce(&[u8]) -> R,
	{
		f(&self.buffer)
	}
}

/// Transmit token for [`VecDevice`].
pub struct VecTxToken<'a> {
	queue: &'a mut VecDeque<Vec<u8>>,
}

impl phy::TxToken for VecTxToken<'_> {
	fn consume<R, F>(self, len: usize, f: F) -> R
	where
		F: FnOnce(&mut [u8]) -> R,
	{
		let mut buffer = vec![0; len];
		let result = f(&mut buffer);
		self.queue.push_back(buffer);
		result
	}
}
