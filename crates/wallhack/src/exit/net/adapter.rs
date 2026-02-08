use std::time::Instant;

use dashmap::DashMap;
use exit_adapter::{
	SocketSet,
	adapter::{
		ExitAdapter, RuntimeError, SendResponse, TcpCloseResponse, TcpListenCloseResponse,
		TcpListenResponse, TcpStreamResponse,
	},
	session::Session,
	session_key::SessionKey,
	sessions,
};

/// Wraps a session with a last-activity timestamp for reaping idle entries.
#[derive(Clone)]
pub struct TimestampedSession {
	pub session: Session,
	pub last_activity: Instant,
}

impl TimestampedSession {
	pub fn new(session: Session) -> Self {
		Self {
			session,
			last_activity: Instant::now(),
		}
	}

	pub fn touch(&mut self) {
		self.last_activity = Instant::now();
	}
}

pub struct SyscallExitAdapter {
	pub sessions: DashMap<SessionKey, TimestampedSession>,
}

impl Default for SyscallExitAdapter {
	fn default() -> Self {
		SyscallExitAdapter {
			sessions: DashMap::new(),
		}
	}
}

impl SyscallExitAdapter {
	#[must_use]
	pub fn new() -> Self {
		SyscallExitAdapter::default()
	}

	/// Start a background task that reaps idle sessions.
	#[must_use]
	pub fn start_reaper(
		&self,
		interval: std::time::Duration,
		ttl: std::time::Duration,
	) -> tokio::task::JoinHandle<()> {
		let sessions = self.sessions.clone();
		tokio::spawn(async move {
			let mut ticker = tokio::time::interval(interval);
			loop {
				ticker.tick().await;
				let before = sessions.len();
				sessions.retain(|_, ts| ts.last_activity.elapsed() < ttl);
				let reaped = before.saturating_sub(sessions.len());
				if reaped > 0 {
					tracing::debug!("Reaped {reaped} idle session(s)");
				}
			}
		})
	}
}

impl ExitAdapter for SyscallExitAdapter {
	async fn udp_send(&self, set: SocketSet, data: &[u8]) -> Result<SendResponse, RuntimeError> {
		self.udp_send_impl(set, data).await
	}

	fn udp_recv_session(
		&self,
		set: SocketSet,
	) -> Result<Option<sessions::udp::UdpSession>, RuntimeError> {
		self.udp_recv_session_impl(set)
	}

	fn tcp_close(&self, set: SocketSet) -> Result<TcpCloseResponse, RuntimeError> {
		self.tcp_close_impl(set)
	}

	// fn tcp_connect_req(&self, set: SocketSet) -> Result<TcpConnectResponse, RuntimeError> {
	// 	self.tcp_connect_req_impl(set)
	// }

	async fn tcp_connect(&self, set: SocketSet) -> Result<TcpStreamResponse, RuntimeError> {
		self.tcp_connect_impl(set).await
	}

	async fn tcp_send(
		&self,
		set: SocketSet,
		buf: &[u8],
		fin: bool,
	) -> Result<SendResponse, RuntimeError> {
		self.tcp_send_impl(set, buf, fin).await
	}

	fn tcp_recv_session(
		&self,
		set: SocketSet,
	) -> Result<Option<sessions::tcp::TcpSession>, RuntimeError> {
		self.tcp_recv_session_impl(set)
	}

	async fn tcp_listen(&self, pair: SocketSet) -> Result<TcpListenResponse, RuntimeError> {
		self.tcp_listen_impl(pair)
	}

	async fn tcp_listen_close(
		&self,
		set: SocketSet,
	) -> Result<TcpListenCloseResponse, RuntimeError> {
		self.tcp_listen_close_impl(set)
	}

	async fn icmp_session(
		&self,
		set: SocketSet,
		ident: u16,
	) -> Result<Option<sessions::icmp::IcmpSession>, RuntimeError> {
		self.icmp_session_impl(set, ident)
	}
}
