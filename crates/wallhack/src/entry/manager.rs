use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use std::sync::atomic::{AtomicBool, Ordering};

use netstack::async_stack::{Netstack, udp_socket::UdpSocketAny};
use smoltcp::phy::Device;
use tokio::time::Instant;
use transport::{BiStream, Transport};

use crate::control::metrics::SharedMetrics;

use super::{actor::TunActor, session::run_tcp_session, udp_session::send_udp_packet};

/// Warn once when connection rate exceeds this threshold (connections/sec)
const HIGH_RATE_THRESHOLD: f64 = 50.0;

/// Window for rate calculation
const RATE_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("netstack error: {0}")]
	Netstack(#[from] netstack::error::Error),

	#[error("session error: {0}")]
	Session(#[from] super::session::Error),

	#[error("udp session error: {0}")]
	UdpSession(#[from] super::udp_session::Error),

	#[error("transport error: {0}")]
	Transport(#[from] transport::TransportError),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

pub struct ConnectionManager<D: Device + Send + 'static, T: Transport + 'static> {
	stack: Netstack<D>,
	transport: Arc<T>,
	metrics: SharedMetrics,
	udp_sessions: HashMap<(smoltcp::wire::IpEndpoint, u16), UdpSession>,
	/// Timestamps of recent TCP connections for rate detection
	recent_connections: Vec<Instant>,
	/// Only warn once about high connection rate
	rate_warned: AtomicBool,
}

impl<T: Transport + 'static> ConnectionManager<super::actor::SmoltcpTunDevice, T> {
	pub fn new(actor: TunActor, transport: Arc<T>, metrics: SharedMetrics) -> Self {
		Self {
			stack: actor.into_stack(),
			transport,
			metrics,
			udp_sessions: HashMap::new(),
			recent_connections: Vec::new(),
			rate_warned: AtomicBool::new(false),
		}
	}
}

#[derive(Debug)]
struct UdpSession {
	local_ip: Option<smoltcp::wire::IpAddress>,
	last_seen: Instant,
}

impl<D: Device + Send + 'static, T: Transport + 'static> ConnectionManager<D, T> {
	pub async fn run(mut self) -> Result<(), Error>
	where
		D: netstack::inner::peek_device::PeekDevice,
	{
		// Use backlog of 1 - JIT creates sockets on-demand anyway
		let mut listener = self.stack.tcp_listen_any(1)?;
		let mut udp = self.stack.udp_bind_any()?;

		let udp_timeout = Duration::from_secs(30);
		let mut udp_buf = vec![0u8; 65535];

		// Channel for UDP responses to send back to clients
		let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<UdpResponse>(64);

		loop {
			tokio::select! {
				stream = listener.accept() => {
					let stream = stream?;
					tracing::debug!(
						local = ?stream.local_endpoint(),
						remote = ?stream.remote_endpoint(),
						"TCP stream accepted, spawning session"
					);

					// Track connection rate for RTFM warning
					let now = Instant::now();
					self.recent_connections.push(now);
					self.recent_connections.retain(|t| now.duration_since(*t) < RATE_WINDOW);
					let rate = self.recent_connections.len() as f64 / RATE_WINDOW.as_secs_f64();
					if rate > HIGH_RATE_THRESHOLD && !self.rate_warned.swap(true, Ordering::Relaxed) {
						tracing::warn!("⚠️  High connection rate detected ({rate:.0}/s)!");
						tracing::warn!("💡 Tip: For scanning (nmap, masscan), use --scan mode for better performance.");
					}

					self.metrics.inc_active_connections();
					let transport = Arc::clone(&self.transport);
					let metrics = self.metrics.clone();
					tokio::spawn(async move {
						let _ = run_tcp_session(stream, transport).await;
						metrics.dec_active_connections();
					});
				}
				result = udp.recv_from(&mut udp_buf) => {
					let (size, meta, local_port) = result?;
					tracing::trace!(
						size,
						local_port,
						remote = %meta.endpoint,
						local_addr = ?meta.local_address,
						"UDP packet received from netstack"
					);
					let key = (meta.endpoint, local_port);
					let now = Instant::now();
					let entry = self.udp_sessions.entry(key).or_insert_with(|| {
						self.metrics.inc_active_flows();
						UdpSession {
							local_ip: meta.local_address,
							last_seen: now,
						}
					});
					entry.last_seen = now;
					// In AnyIP mode:
					// - meta.local_address = destination the client wanted (target)
					// - meta.endpoint = client's source address
					let target = meta
						.local_address.map_or_else(|| format!("0.0.0.0:{local_port}"), |a| smoltcp::wire::IpEndpoint::new(a, local_port).to_string());
					let source = meta.endpoint.to_string();
					let client_endpoint = meta.endpoint;
					let local_addr = meta.local_address;
					let payload = udp_buf[..size].to_vec();
					let transport = Arc::clone(&self.transport);
					let response_tx = response_tx.clone();
					tokio::spawn(async move {
						match send_udp_packet(transport, &target, &source, &payload).await {
							Ok(response) if !response.is_empty() => {
								// Queue response to be sent back to client
								let _ = response_tx.send(UdpResponse {
									local_port,
									data: response,
									client_endpoint,
									local_addr,
								}).await;
							}
							Ok(_) => {
								tracing::trace!("Empty UDP response from exit");
							}
							Err(e) => {
								tracing::warn!("UDP forward failed: {e}");
							}
						}
					});
				}
				Some(response) = response_rx.recv() => {
					// Send queued UDP response back to client
					let response_meta = smoltcp::socket::udp::UdpMetadata {
						endpoint: response.client_endpoint,
						local_address: response.local_addr,
						meta: smoltcp::phy::PacketMeta::default(),
					};
					if let Err(e) = udp.send_to(response.local_port, &response.data, response_meta) {
						tracing::warn!("Failed to send UDP response to client: {e}");
					} else {
						tracing::trace!(local_port = response.local_port, client = %response.client_endpoint, "UDP response sent to client");
					}
				}
				result = self.transport.accept_bi() => {
					let Some(mut stream) = result? else {
						return Ok(());
					};
					let metrics = self.metrics.clone();
					let sessions = std::mem::take(&mut self.udp_sessions);
					let (sessions, result) = handle_udp_stream(&mut udp, sessions, metrics, &mut stream).await?;
					self.udp_sessions = sessions;
					if let Err(e) = result {
						tracing::warn!("udp stream handling failed: {e}");
					}
				}
				() = tokio::time::sleep(Duration::from_secs(5)) => {
					let now = Instant::now();
					let metrics = self.metrics.clone();
					self.udp_sessions.retain(|_, session| {
						let keep = now.duration_since(session.last_seen) < udp_timeout;
						if !keep {
							metrics.dec_active_flows();
						}
						keep
					});
				}
			}
		}
	}
}

struct UdpResponse {
	local_port: u16,
	data: Vec<u8>,
	client_endpoint: smoltcp::wire::IpEndpoint,
	local_addr: Option<smoltcp::wire::IpAddress>,
}

async fn handle_udp_stream<D: Device + Send + 'static, S: BiStream>(
	udp: &mut UdpSocketAny<D>,
	mut sessions: HashMap<(smoltcp::wire::IpEndpoint, u16), UdpSession>,
	metrics: SharedMetrics,
	stream: &mut S,
) -> Result<
	(
		HashMap<(smoltcp::wire::IpEndpoint, u16), UdpSession>,
		Result<(), Error>,
	),
	Error,
> {
	let init = crate::transport::bridge::read_length_delimited::<protobuf::v2::SessionInit, _>(
		stream,
		crate::transport::bridge::SESSION_INIT_MTU,
	)
	.await?;
	if init.protocol != protobuf::v2::SessionProtocol::Udp as i32 {
		return Ok((sessions, Ok(())));
	}
	let target: SocketAddr = init
		.target_addr
		.parse()
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
	let remote = smoltcp::wire::IpEndpoint::from(target);

	let key = sessions
		.keys()
		.find(|(endpoint, _)| *endpoint == remote)
		.copied();
	let Some((endpoint, local_port)) = key else {
		return Ok((sessions, Ok(())));
	};

	let session = sessions.get_mut(&(endpoint, local_port));
	let Some(session) = session else {
		return Ok((sessions, Ok(())));
	};
	session.last_seen = Instant::now();
	let local_ip = session.local_ip;
	let mut payload = Vec::new();
	tokio::io::AsyncReadExt::read_to_end(stream, &mut payload).await?;
	if payload.is_empty() {
		return Ok((sessions, Ok(())));
	}
	let meta = smoltcp::socket::udp::UdpMetadata {
		endpoint: remote,
		local_address: local_ip,
		meta: smoltcp::phy::PacketMeta::default(),
	};
	let send_result = udp.send_to(local_port, &payload, meta).map_err(Error::from);
	if send_result.is_ok() {
		metrics.inc_packets_in(1);
		metrics.inc_bytes_in(payload.len() as u64);
	}
	Ok((sessions, send_result))
}
