use std::{
	collections::HashMap,
	net::SocketAddr,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	time::Duration,
};

use netstack::async_stack::{HeldSyn, Netstack, SynProxyState, udp_socket::UdpSocketAny};
use smoltcp::phy::Device;
use tokio::{io::unix::AsyncFd, sync::Notify, time::Instant};
use transport::{BiStream, Transport};

use crate::control::metrics::SharedMetrics;

use super::{
	actor::TunActor,
	icmp::{IcmpUnreachableReason, build_icmp_dest_unreachable},
	session::run_tcp_session,
	syn_proxy::{parse_syn_target, probe_tcp_target},
	udp_session::{UdpForwardResult, send_udp_packet},
};

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
	tun_writer: Arc<AsyncFd<tun::Device>>,
	udp_sessions: HashMap<(smoltcp::wire::IpEndpoint, u16), UdpSession>,
	/// Timestamps of recent TCP connections for rate detection
	recent_connections: Vec<Instant>,
	/// Only warn once about high connection rate
	rate_warned: AtomicBool,
	/// SYN proxy: receive held SYN packets from the poll loop.
	syn_rx: tokio::sync::mpsc::UnboundedReceiver<HeldSyn>,
	/// SYN proxy: send re-injected packets back to the poll loop.
	inject_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
	/// SYN proxy: wake the poll loop when a probe completes.
	wake_notify: Arc<Notify>,
	/// SYN proxy: shared state for fast mode toggle and port cache.
	syn_proxy_state: Arc<SynProxyState>,
}

impl<T: Transport + 'static> ConnectionManager<super::actor::SmoltcpTunDevice, T> {
	pub fn new(
		actor: TunActor,
		transport: Arc<T>,
		metrics: SharedMetrics,
		fast_mode: bool,
	) -> (Self, Arc<SynProxyState>) {
		let (mut stack, tun_writer) = actor.into_stack();
		let state = Arc::new(SynProxyState::new(fast_mode));
		let (syn_rx, inject_tx, wake_notify) = stack.set_syn_proxy(Arc::clone(&state));

		let manager = Self {
			stack,
			transport,
			metrics,
			tun_writer,
			udp_sessions: HashMap::new(),
			recent_connections: Vec::new(),
			rate_warned: AtomicBool::new(false),
			syn_rx,
			inject_tx,
			wake_notify,
			syn_proxy_state: Arc::clone(&state),
		};
		(manager, state)
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
					let count = u32::try_from(self.recent_connections.len()).unwrap_or(u32::MAX);
					let rate = f64::from(count) / RATE_WINDOW.as_secs_f64();
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
					let tun_writer = Arc::clone(&self.tun_writer);
					tokio::spawn(async move {
						match send_udp_packet(transport, &target, &source, &payload).await {
							Ok(UdpForwardResult::Response(data)) if !data.is_empty() => {
								let _ = response_tx.send(UdpResponse {
									local_port,
									data,
									client_endpoint,
									local_addr,
								}).await;
							}
							Ok(UdpForwardResult::Response(_) | UdpForwardResult::Timeout) => {
								tracing::trace!("Empty/timeout UDP response from exit");
							}
							Ok(result @ (UdpForwardResult::PortUnreachable
								| UdpForwardResult::HostUnreachable
								| UdpForwardResult::NetUnreachable)) => {
								let reason = match result {
									UdpForwardResult::PortUnreachable => IcmpUnreachableReason::Port,
									UdpForwardResult::HostUnreachable => IcmpUnreachableReason::Host,
									UdpForwardResult::NetUnreachable => IcmpUnreachableReason::Net,
									_ => unreachable!(),
								};
								if let (Some(target_ip), Some(client_ip)) = (local_addr, Some(client_endpoint.addr))
									&& let Some(packet) = build_icmp_dest_unreachable(
										reason,
										client_ip,
										target_ip,
										local_port,
										client_endpoint.port,
										&payload,
									)
								{
									if let Err(e) = tun_writer.get_ref().send(&packet) {
										tracing::warn!("Failed to inject ICMP packet: {e}");
									} else {
										tracing::trace!(?reason, "Injected ICMP unreachable into TUN");
									}
								}
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
				Some(held) = self.syn_rx.recv() => {
					let transport = Arc::clone(&self.transport);
					let state = Arc::clone(&self.syn_proxy_state);
					let inject_tx = self.inject_tx.clone();
					let wake = Arc::clone(&self.wake_notify);
					let port = held.dst_port;
					tokio::spawn(async move {
						let Some(target_addr) = parse_syn_target(&held.packet) else {
							tracing::debug!(port, "SYN probe: failed to parse target");
							state.mark_closed(port);
							wake.notify_one();
							return;
						};
						let open = probe_tcp_target(&transport, &target_addr).await;
						if open {
							tracing::debug!(port, "SYN probe: open");
							state.mark_open(port);
							// Re-inject the original SYN so the poll loop can JIT-bind + process it.
							let _ = inject_tx.send(held.packet);
						} else {
							tracing::debug!(port, "SYN probe: closed");
							state.mark_closed(port);
							// Re-inject so smoltcp sees the SYN with no listener → RST.
							let _ = inject_tx.send(held.packet);
						}
						wake.notify_one();
					});
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
