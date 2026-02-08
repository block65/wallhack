use std::{
	io,
	mem::MaybeUninit,
	net::{Ipv6Addr, SocketAddr},
};

use smoltcp::{
	phy::ChecksumCapabilities,
	wire::{Icmpv4Packet, Icmpv4Repr, Icmpv6Packet, Icmpv6Repr},
};

use crate::{SocketSet, adapter::RuntimeError};

use super::common::{RxSession, SessionStatus};

#[derive(Debug, Clone)]
pub struct IcmpSession {
	socket: std::sync::Arc<tokio::io::unix::AsyncFd<socket2::Socket>>,
	pair: SocketSet,
}

impl IcmpSession {
	#[must_use]
	pub fn new(socket: tokio::io::unix::AsyncFd<socket2::Socket>, pair: SocketSet) -> Self {
		Self {
			socket: std::sync::Arc::new(socket),
			pair,
		}
	}

	pub async fn echo_request(
		&self,
		data: &[u8],
		seq_no: u16,
		// ident: u16,
		recv_buf: &mut [u8],
	) -> Result<SessionStatus, RuntimeError> {
		tracing::trace!(seq_no = seq_no, "Sending ICMP echo request");
		let default_caps = ChecksumCapabilities::default();

		let echo_request_buf = match self.pair {
			SocketSet::Ipv4(_) => {
				let icmp_repr = Icmpv4Repr::EchoRequest {
					ident: 0x0, // ident is ignored and assigned by the OS instead
					seq_no,
					data,
				};

				let mut buf = vec![0u8; icmp_repr.buffer_len()];
				let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut buf);
				icmp_repr.emit(&mut icmp_packet, &default_caps);

				buf
			}
			SocketSet::Ipv6((_, dst_addr6)) => {
				let icmp_repr = Icmpv6Repr::EchoRequest {
					ident: 0x0, // ident is ignored and assigned by the OS instead
					seq_no,
					data,
				};
				let mut buf = vec![0u8; icmp_repr.buffer_len()];
				let mut icmp_packet = Icmpv6Packet::new_unchecked(&mut buf);
				icmp_repr.emit(
					&Ipv6Addr::UNSPECIFIED,
					dst_addr6.ip(),
					&mut icmp_packet,
					&default_caps,
				);
				buf
			}
		};

		let (_, dst_addr) = self.pair.into();
		let status = self.send(dst_addr, &echo_request_buf).await?;

		tracing::trace!("Sent ICMP echo request status {:?}. Waiting to rx", status);

		// NOTE: this will wait forever until data is received
		self.recv(recv_buf).await
	}
}

impl RxSession for IcmpSession {
	async fn send(
		&self,
		dst_addr: SocketAddr,
		buf: &[u8],
	) -> Result<SessionStatus, RuntimeError> {
		let dst_addr2: socket2::SockAddr = dst_addr.into();

		loop {
			tracing::trace!("waiting to send some data to {:?}", dst_addr);

			let mut guard = self.socket.writable().await?;

			// Attempt to send the data
			tracing::trace!("Attempting to send data to {:?}", dst_addr);

			match guard.try_io(|inner| inner.get_ref().send_to(buf, &dst_addr2)) {
				Ok(Ok(bytes_sent)) => {
					tracing::trace!("Successfully sent {} bytes to {:?}", bytes_sent, dst_addr);
					return Ok(SessionStatus::DataIo { size: bytes_sent });
				}
				Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {
					// The socket wasn't actually ready (spurious wakeup or EAGAIN). Clear
					// the readiness state and loop again to wait for readiness.
					tracing::warn!("Send operation would block, retrying.");
					guard.clear_ready();
					// Optional: Yield to allow other tasks to run, preventing potential
					// busy-looping if the socket remains not ready for a while.
					// tokio::task::yield_now().await; Retry writable().await
					// continue;
				}
				Ok(Err(e)) => {
					tracing::error!(ip=?dst_addr2, error=%e, "Send operation failed");
					return Err(RuntimeError::Io(e));
				}
				Err(_readiness_error) => {
					// The readiness check itself failed (rare). This indicates an issue
					// with the underlying readiness mechanism.
					tracing::warn!("Readiness check failed after writable().await, retrying.");
					// Loop will retry writable().await
					// continue;
				}
			}
		}
	}

	async fn recv(&self, buf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
		// Convert &mut [u8] to &mut [MaybeUninit<u8>] safely
		// SAFETY: [u8] and [MaybeUninit<u8>] have identical layout,
		// and we're allowed to write MaybeUninit over initialized memory.
		#[allow(unsafe_code)]
		let uninit_buf: &mut [MaybeUninit<u8>] =
			unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast(), buf.len()) };

		loop {
			tracing::trace!("Waiting for (more) data from socket");
			let mut guard = self.socket.readable().await?;

			let io_result = match guard.try_io(|inner| inner.get_ref().recv(uninit_buf)) {
				Ok(result) => result,
				Err(_) => continue, // spurious wakeup
			};

			match io_result {
				Ok(0) => {
					tracing::trace!("peer closed connection");
					return Ok(SessionStatus::PeerClosed);
				}
				Ok(n) => {
					tracing::trace!("received {} bytes", n);
					return Ok(SessionStatus::DataIo { size: n });
				}
				Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("would block, retrying");
					guard.clear_ready();
				}
				Err(e) => {
					tracing::error!(error=%e, "receive failed");
					return Err(RuntimeError::Io(e));
				}
			}
		}
	}
}
