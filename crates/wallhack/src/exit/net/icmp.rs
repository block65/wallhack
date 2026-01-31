use std::{
	io,
	net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
	os::fd::AsRawFd,
};

use super::adapter::SyscallExitAdapter;

use exit_adapter::{
	SocketSet,
	adapter::RuntimeError,
	session::Session,
	session_key::SessionKey,
	sessions::{self, icmp::IcmpSession},
};
use tokio::io::unix::AsyncFd;

pub fn create_async(local_addr: IpAddr) -> io::Result<AsyncFd<socket2::Socket>> {
	let domain = if local_addr.is_ipv4() {
		socket2::Domain::IPV4
	} else {
		socket2::Domain::IPV6
	};
	let protocol = if local_addr.is_ipv4() {
		socket2::Protocol::ICMPV4
	} else {
		socket2::Protocol::ICMPV6
	};
	let socket_type = socket2::Type::DGRAM;

	let socket = match socket2::Socket::new(domain, socket_type, Some(protocol)) {
		Ok(sock) => {
			tracing::trace!(
				"Successfully created ICMP datagram socket (fd: {})",
				sock.as_raw_fd()
			);
			sock
		}
		Err(err) => {
			tracing::error!("Failed to create ICMP datagram socket: {err}");
			if err.kind() == io::ErrorKind::PermissionDenied {
				tracing::error!(
					"Hint: This might be due to permissions. Check kernel's net.ipv4.ping_group_range or run with CAP_NET_RAW."
				);
			}
			return Err(err);
		}
	};

	if let Err(err) = socket.set_nonblocking(true) {
		tracing::error!(
			"Failed to set socket non-blocking (fd: {}): {}",
			socket.as_raw_fd(),
			err
		);
		return Err(err);
	}
	tracing::trace!("Made socket non-blocking (fd: {})", socket.as_raw_fd());

	if let Err(err) = socket.set_send_buffer_size(8192) {
		tracing::warn!("Failed to set send buffer size: {}", err);
	} else {
		tracing::trace!("Set send buffer size to 8192");
	}

	if let Err(err) = socket.set_recv_buffer_size(8192) {
		tracing::warn!("Failed to set receive buffer size: {}", err);
	} else {
		tracing::trace!("Set receive buffer size to 8192");
	}

	if let Err(err) = socket.set_keepalive(true) {
		tracing::warn!("Failed to set SO_KEEPALIVE: {}", err);
	}

	let bind_addr = if local_addr.is_ipv4() {
		SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
	} else {
		SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
	};
	if let Err(err) = socket.bind(&bind_addr.into()) {
		tracing::error!(
			"Failed to bind ICMP socket to {:?} (fd: {}): {}",
			bind_addr,
			socket.as_raw_fd(),
			err
		);
		if err.kind() == io::ErrorKind::PermissionDenied {
			tracing::error!(
				"Hint: Binding error might also indicate permission issues. {:?}",
				err
			);
		}
		return Err(err);
	}
	tracing::trace!(
		"Bound socket to {:?} (fd: {})",
		bind_addr,
		socket.as_raw_fd()
	);

	let async_fd = match AsyncFd::new(socket) {
		Ok(fd) => {
			tracing::trace!("Wrapped socket in AsyncFd (fd: {})", fd.as_raw_fd());
			fd
		}
		Err(err) => {
			tracing::error!("Failed to wrap socket in AsyncFd: {err}");
			return Err(err);
		}
	};

	Ok(async_fd)
}

impl SyscallExitAdapter {
	pub fn icmp_session_impl(
		&self,
		set: SocketSet,
		ident: u16,
	) -> Result<Option<IcmpSession>, RuntimeError> {
		tracing::trace!("Received send data request: {:?}", set);

		let key = SessionKey::Icmp((set, ident));

		if self.sessions.contains_key(&key) {
			false
		} else {
			let local_addr = match set {
				SocketSet::Ipv4(_) => std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
					std::net::Ipv4Addr::UNSPECIFIED,
					0,
				)),
				SocketSet::Ipv6(_) => std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
					std::net::Ipv6Addr::UNSPECIFIED,
					0,
					0,
					0,
				)),
			};

			let socket = create_async(local_addr.ip())?;
			let session = Session::Icmp(sessions::icmp::IcmpSession::new(socket, set));
			self.sessions.insert(key.clone(), session);
			true
		};

		let maybe_session = self.sessions.get(&key);
		tracing::trace!("maybe_session: {:?}", maybe_session);
		match maybe_session {
			Some(session) => {
				if let Session::Icmp(session) = session.value() {
					Ok(Some(session.clone()))
				} else {
					// non-icmp session - should not happen
					Err(RuntimeError::SessionInvalid(key))
				}
			}
			None => Ok(None),
		}
	}
}
