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
		tracing::debug!(seq_no = seq_no, "Sending ICMP echo request");
		let default_caps = ChecksumCapabilities::default();

		let mut echo_request_buf = match self.pair {
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
		let status = self.send(dst_addr, &mut echo_request_buf).await?;

		tracing::debug!("Sent ICMP echo request status {:?}. Waiting to rx", status);

		// NOTE: this will wait forever until data is received
		self.recv(recv_buf).await
	}
}

impl RxSession for IcmpSession {
	async fn send(
		&self,
		dst_addr: SocketAddr,
		buf: &mut [u8],
	) -> Result<SessionStatus, RuntimeError> {
		let dst_addr2: socket2::SockAddr = dst_addr.into();

		loop {
			let mut guard = self.socket.writable().await?;

			match guard.try_io(|inner| inner.get_ref().send_to(buf, &dst_addr2)) {
				Ok(Ok(bytes_sent)) => {
					tracing::trace!("Successfully sent {} bytes to {:?}", bytes_sent, dst_addr);
					return Ok(SessionStatus::DataIo { size: bytes_sent });
				}
				Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {
					// The socket wasn't actually ready (spurious wakeup or EAGAIN). Clear
					// the readiness state and loop again to wait for readiness.
					tracing::trace!("Send operation would block, retrying.");
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

	async fn recv(&self, buf_wtf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
		// WARN: I dont know how to use MaybeUninit, and the trait is `buf mut [u8]`
		// which I dont want to change, so we do this absolute garbage for now.
		// https://github.com/rust-lang/socket2/issues/270
		// let mut wtf_buffer = buf.to_vec();
		let mut wtf_recv_buffer = [MaybeUninit::<u8>::uninit(); 1500];

		loop {
			tracing::trace!("Waiting for (more) data from socket");
			let mut guard = self.socket.readable().await?;

			match guard.try_io(|inner| inner.get_ref().recv(&mut wtf_recv_buffer)) {
				Ok(Ok(n)) => {
					// SAFETY: just received into the `buffer`.
					let wtf_initialized_part = unsafe {
						std::slice::from_raw_parts(wtf_recv_buffer.as_ptr().cast::<u8>(), n)
					};

					tracing::trace!("received {} bytes into {:?}", n, wtf_initialized_part);

					// TODO: WTF
					buf_wtf[..n].copy_from_slice(wtf_initialized_part);
					return Ok(SessionStatus::DataIo { size: n });
				}
				Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {
					tracing::trace!(error=%e, "Receive operation would block, retrying");
					guard.clear_ready();
					// continue;
				}
				Ok(Err(e)) => {
					tracing::error!(error=%e, "Receive operation failed");
					return Err(RuntimeError::Io(e));
				}
				Err(e) => {
					tracing::warn!(
						error=?e,
						"Readiness check failed after readable().await, retrying",
					);
					// continue;
				}
			}
		}
	}
}
