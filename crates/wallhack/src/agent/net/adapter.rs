use agent_adapter::{
	SocketSet,
	adapter::{
		AgentAdapter, RuntimeError, SendResponse, TcpCloseResponse, TcpConnectResponse,
		TcpListenCloseResponse, TcpListenResponse,
	},
	session::Session,
	session_key::SessionKey,
	sessions,
};
use dashmap::DashMap;

pub struct SyscallAgentAdapter {
	pub sessions: DashMap<SessionKey, Session>,
}

impl Default for SyscallAgentAdapter {
	fn default() -> Self {
		SyscallAgentAdapter {
			sessions: DashMap::new(),
		}
	}
}

impl SyscallAgentAdapter {
	#[must_use]
	pub fn new() -> Self {
		SyscallAgentAdapter::default()
	}
}

impl AgentAdapter for SyscallAgentAdapter {
	async fn udp_send(
		&self,
		set: SocketSet,
		data: &mut [u8],
	) -> Result<SendResponse, RuntimeError> {
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

	async fn tcp_connect(&self, set: SocketSet) -> Result<TcpConnectResponse, RuntimeError> {
		self.tcp_connect_impl(set).await
	}

	async fn tcp_send(&self, set: SocketSet, buf: Vec<u8>) -> Result<SendResponse, RuntimeError> {
		self.tcp_send_impl(set, buf).await
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
// WARNING: This file contains AI-generated edits
