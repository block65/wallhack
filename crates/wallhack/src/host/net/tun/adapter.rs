use super::{
	icmp::{IcmpFlow, IcmpFlowHashKey, icmp_repr},
	ip_packet::{self, IpPacket},
	tcp::{TcpFlow, TcpFlowHashKey},
	udp::{UdpFlow, UdpFlowHashKey, emit_udp_segment},
};
use crate::host::{adapter::HostAdapter, net::tun::tcp::TcpFlowState};
use dashmap::DashMap;
use protobuf::{
	SocketSet,
	v2::{self, SocketV6Address},
};
use rand::Rng;
use smoltcp::{
	phy::ChecksumCapabilities,
	wire::{
		IcmpRepr, Icmpv4Packet, Icmpv4Repr, Icmpv6Packet, Icmpv6Repr, IpAddress, IpProtocol,
		Ipv4Packet, Ipv6Packet, TcpControl, TcpPacket, TcpSeqNumber, UdpPacket,
	},
};
use std::{env, net::SocketAddrV4, ops::Add, sync::Arc};
use tun::{AsyncDevice, Device};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("io error - {0}")]
	Io(#[from] std::io::Error),

	#[error("instruction error - {0}")]
	Instruction(#[from] ip_packet::Error),

	#[error("{source} - interface {name}")]
	Interface {
		source: std::io::Error,
		name: String,
	},

	#[error("{source} - interface {name}")]
	AsyncTun {
		source: std::io::Error,
		name: String,
	},

	#[error("{source} Is this really a TUN device?")]
	InvalidArgument { source: std::io::Error },

	#[error("custom error: {0}")]
	Unsupported(String),

	// protobuf::ConversionError
	#[error("conversion error: {0}")]
	Conversion(#[from] protobuf::ConversionError),

	// smoltcp::wire::Error
	#[error("smoltcp error: {0}")]
	Smoltcp(#[from] smoltcp::wire::Error),

	//tun::Error
	#[error("tun error: {0:?}")]
	Tun(#[from] tun::Error),
}

#[derive(Clone)]
pub struct TunAdapter {
	pub name: String,
	inner: Arc<tun::AsyncDevice>,
	tcp_flows: DashMap<TcpFlowHashKey, TcpFlow>,
	udp_flows: DashMap<UdpFlowHashKey, UdpFlow>,
	icmp_flows: DashMap<IcmpFlowHashKey, IcmpFlow>,
}

const IFACE_NAME_CHARSET: &[u8] = b"dfgjpqstz2346789";
const IFACE_NAME_SUFFIX_LEN: usize = 4;
fn random_iface_name() -> String {
	let mut rng = rand::rng();

	let index = rng.random_range(0..9);

	let rand: String = (0..IFACE_NAME_SUFFIX_LEN)
		.map(|_| {
			let idx = rng.random_range(0..IFACE_NAME_CHARSET.len());
			IFACE_NAME_CHARSET[idx] as char
		})
		.collect();

	// strip non-alphanumeric from package name
	let prefix = env::var("CARGO_PKG_NAME")
		.unwrap_or_else(|_| "tun".to_string())
		.chars()
		.filter(char::is_ascii_alphanumeric)
		.collect::<String>();

	format!("{prefix}{index}{rand}")
}

impl TunAdapter {
	pub fn try_new(maybe_if_name: Option<String>) -> Result<Self, Error> {
		let if_name = if let Some(name) = maybe_if_name {
			name
		} else {
			random_iface_name()
		};

		tracing::trace!("tun interface is {:?}", if_name);

		let mut config = tun::Configuration::default();
		config.tun_name(if_name.clone());

		let dev = Device::new(&config)?;
		let async_dev = AsyncDevice::new(dev)?;

		Ok(Self {
			name: if_name,
			inner: Arc::new(async_dev),
			tcp_flows: DashMap::new(),
			udp_flows: DashMap::new(),
			icmp_flows: DashMap::new(),
		})
	}
}

impl TunAdapter {
	pub fn handle_ipv4_packet(
		&self,
		ipv4_packet: &Ipv4Packet<&[u8]>,
	) -> Result<Vec<v2::tunnel_message::Message>, Error> {
		let src_ip_addr = ipv4_packet.src_addr();
		let dst_ip_addr = ipv4_packet.dst_addr();
		let payload = ipv4_packet.payload();

		let default_caps = ChecksumCapabilities::default();

		let mut messages_to_send: Vec<v2::tunnel_message::Message> = Vec::new();

		match ipv4_packet.next_header() {
			IpProtocol::Udp => {
				let udp_packet = UdpPacket::new_checked(payload)?;

				let src_sock = SocketAddrV4::new(src_ip_addr, udp_packet.src_port());
				let dst_sock = SocketAddrV4::new(dst_ip_addr, udp_packet.dst_port());
				let pair = SocketSet::Ipv4((src_sock, dst_sock));

				self.udp_flows.insert(pair, UdpFlow);

				messages_to_send.push(
					v2::host_instruction::Instruction::UdpSend(v2::UdpSendInstruction {
						pair: Some(pair.into()),
						data: udp_packet.payload().to_vec(),
					})
					.into(),
				);
			}

			IpProtocol::Icmp => {
				let icmp_packet = Icmpv4Packet::new_checked(payload)?;
				if let Icmpv4Repr::EchoRequest {
					ident,
					seq_no,
					data,
				} = Icmpv4Repr::parse(&icmp_packet, &default_caps)?
				{
					let src_addr_pb = v2::SocketV4Address {
						ip: Some(src_ip_addr.into()),
						..Default::default()
					};
					let dst_addr_pb = v2::SocketV4Address {
						ip: Some(dst_ip_addr.into()),
						..Default::default()
					};

					messages_to_send.push(
						v2::host_instruction::Instruction::IcmpSend(v2::IcmpSendInstruction {
							pair: Some((src_addr_pb, dst_addr_pb).into()),

							icmp_message: Some(
								v2::icmp_send_instruction::IcmpMessage::IcmpEchoRequest(
									v2::IcmpEchoRequest {
										data: data.to_vec(),
										ident: u32::from(ident),
										seq_no: u32::from(seq_no),
									},
								),
							),
						})
						.into(),
					);
				} else {
					tracing::warn!("Unsupported ICMPv4 type: {:?}", icmp_packet);
					return Ok(vec![]);
				}
			}

			// Handle TCP packets
			IpProtocol::Tcp => {
				let tcp_packet = match TcpPacket::new_checked(payload) {
					Ok(p) => p,
					Err(e) => {
						tracing::error!("Failed to parse TCP packet: {e}");
						return Ok(vec![]);
					}
				};

				let set = SocketSet::Ipv4((
					SocketAddrV4::new(src_ip_addr, tcp_packet.src_port()),
					SocketAddrV4::new(dst_ip_addr, tcp_packet.dst_port()),
				));

				// this inserts a new flow if this is the first packet for this set
				let mut flow = self.tcp_flows.entry(set).or_insert_with(|| TcpFlow {
					ack_for_client_seq: tcp_packet.seq_number(),
					host_advertised_window: tcp_packet.window_len(),
					host_current_seq: tcp_packet.seq_number(),
					..Default::default()
				});

				// This is the very start of a connection.
				if tcp_packet.syn() && !tcp_packet.ack() {
					flow.ack_for_client_seq = tcp_packet.seq_number().add(1);
					flow.client_advertised_window = tcp_packet.window_len();
					flow.connection_state = TcpFlowState::SynReceived;

					messages_to_send.push(
						v2::host_instruction::Instruction::TcpConnect(v2::TcpConnectInstruction {
							pair: Some(set.into()),
						})
						.into(),
					);
				} else
				// push data
				if tcp_packet.psh() && tcp_packet.ack() {
					let data_len = tcp_packet.payload().len();

					tracing::trace!("IPV4 TCP PSH from {set} with {data_len} bytes. TcpSend.");

					// This is the next seq we expect from the TCP stack.
					flow.ack_for_client_seq =
						tcp_packet.seq_number().add(tcp_packet.payload().len());

					messages_to_send.push(
						v2::host_instruction::Instruction::TcpSend(v2::TcpSendInstruction {
							pair: Some(set.into()),
							data: tcp_packet.payload().to_vec(),
						})
						.into(),
					);
				} else
				// A FINACK from the host adapter indicates they got the data and are
				// closing the connection.
				if tcp_packet.fin() && tcp_packet.ack() {
					tracing::trace!(
						"IPV4 TCP FINACK from {set}. Client SEQ: {:?}, Current flow state: {:?}",
						tcp_packet.seq_number(),
						flow.connection_state
					);

					// Always ACK the client's FIN by setting the expected next sequence
					// number.
					flow.ack_for_client_seq = tcp_packet.seq_number().add(1);

					// We always send an ACK for the client's FIN. The `SendOk` response
					// type will generate a pure ACK packet.
					let ack_for_client_fin_msg = v2::AgentResponse {
						pair: Some(set.into()),
						response: Some(v2::agent_response::Response::TcpResponse(
							v2::TcpResponse {
								response: Some(v2::tcp_response::Response::Ok(
									v2::TcpOkResponse {},
								)),
							},
						)),
					};
					messages_to_send.push(ack_for_client_fin_msg.into());

					let close_instruction_payload =
						v2::host_instruction::Instruction::TcpClose(v2::TcpCloseInstruction {
							pair: Some(set.into()),
						});
					messages_to_send.push(
						v2::HostInstruction {
							instruction: Some(close_instruction_payload),
						}
						.into(),
					);
				} else if tcp_packet.rst() {
					// RST tears down the connection immediately. No instruction needed.
					tracing::trace!("IPV4 TCP RST from {set}. Removing flow.");
					self.tcp_flows.remove(&set);
				} else if tcp_packet.ack() && flow.connection_state == TcpFlowState::FinWait2 {
					// Last ACK is sent after a FIN, and we can remove the flow.
					tracing::trace!("IPV4 TCP ACK from {set} in LastAck state. Removing flow.");
					self.tcp_flows.remove(&set);
				} else {
					// Any other packet (e.g., a pure ACK) requires no instruction for the
					// agent. We just let it pass without action. The TCP flow state is
					// not updated by pure ACKs in this logic, which is a simplification
					// we are keeping for now.
					tracing::debug!(
						"IPV4 TCP packet from {set} requires no action: flags: SYN={}, ACK={}, FIN={}, RST={}, PSH={}, URG={}",
						tcp_packet.syn(),
						tcp_packet.ack(),
						tcp_packet.fin(),
						tcp_packet.rst(),
						tcp_packet.psh(),
						tcp_packet.urg()
					);
				}
			}
			_ => {
				tracing::warn!("Unsupported IPv4 protocol: {:?}", ipv4_packet.next_header());
			}
		}

		Ok(messages_to_send)
	}

	pub fn handle_ipv6_packet(
		&self,
		ipv6_packet: &Ipv6Packet<&[u8]>,
	) -> Result<Vec<v2::tunnel_message::Message>, Error> {
		let src_ip = ipv6_packet.src_addr();
		let dst_ip = ipv6_packet.dst_addr();
		let payload = ipv6_packet.payload();

		let default_caps = ChecksumCapabilities::default();

		let mut messages_to_send: Vec<v2::tunnel_message::Message> = Vec::new();

		match ipv6_packet.next_header() {
			IpProtocol::Udp => {
				let udp_packet = UdpPacket::new_checked(payload)?;
				let src_addr_pb = SocketV6Address {
					ip: Some(src_ip.into()),
					port: u32::from(udp_packet.src_port()),
					..Default::default()
				};
				let dst_addr_pb = SocketV6Address {
					ip: Some(dst_ip.into()),
					port: u32::from(udp_packet.dst_port()),
					..Default::default()
				};

				messages_to_send.push(
					v2::host_instruction::Instruction::UdpSend(v2::UdpSendInstruction {
						pair: Some((src_addr_pb, dst_addr_pb).into()),
						data: udp_packet.payload().to_vec(),
					})
					.into(),
				);
			}
			IpProtocol::Icmpv6 => {
				let icmp_packet = Icmpv6Packet::new_checked(payload)?;

				match Icmpv6Repr::parse(
					&ipv6_packet.src_addr(),
					&ipv6_packet.dst_addr(),
					&icmp_packet,
					&default_caps,
				)? {
					Icmpv6Repr::EchoRequest {
						ident,
						seq_no,
						data,
					} => {
						let src_addr_pb = SocketV6Address {
							ip: Some(src_ip.into()),
							..Default::default()
						};
						let dst_addr_pb = SocketV6Address {
							ip: Some(dst_ip.into()),
							..Default::default()
						};

						let instruction =
							v2::host_instruction::Instruction::IcmpSend(v2::IcmpSendInstruction {
								pair: Some((src_addr_pb, dst_addr_pb).into()),
								icmp_message: Some(
									v2::icmp_send_instruction::IcmpMessage::IcmpEchoRequest(
										v2::IcmpEchoRequest {
											data: data.to_vec(),
											ident: u32::from(ident),
											seq_no: u32::from(seq_no),
										},
									),
								),
							});

						messages_to_send.push(instruction.into());
					}
					Icmpv6Repr::EchoReply { .. } => {
						tracing::warn!("ICMPv6 Echo Reply is not handled in this context.");
					}
					_ => {
						tracing::warn!("Unsupported ICMPv6 type: {:?}", icmp_packet);
					}
				}
			}
			IpProtocol::Tcp => {
				tracing::info!("IPV6 TCP mapping not implemented.");
			}
			_ => {
				tracing::warn!("Unsupported IPv6 protocol: {:?}", ipv6_packet.next_header());
			}
		}

		Ok(messages_to_send)
	}
}

enum Repr<'a> {
	Tcp(smoltcp::wire::TcpRepr<'a>),
	Icmp(smoltcp::wire::IcmpRepr<'a>),
	Udp(smoltcp::wire::UdpRepr),
}

macro_rules! tcp_repr_default {
	($flow:expr, $src_port:expr, $dst_port:expr, $control:expr, $payload:expr) => {{
		Repr::Tcp(smoltcp::wire::TcpRepr {
			control: $control,
			seq_number: $flow.host_current_seq,
			ack_number: Some($flow.ack_for_client_seq),
			window_len: $flow.host_advertised_window,
			window_scale: None,
			max_seg_size: None,
			sack_permitted: false,
			sack_ranges: [None; 3],
			payload: $payload,
			src_port: $src_port,
			dst_port: $dst_port,
			timestamp: None,
		})
	}};
}

impl HostAdapter for TunAdapter {
	type Error = Error;

	async fn next_message(
		&self,
		buf: &mut [u8],
	) -> Result<Vec<v2::tunnel_message::Message>, Self::Error> {
		tracing::trace!("Waiting for next message from host adapter...");

		let n = self.inner.recv(buf).await?;

		tracing::trace!("Received {} bytes from host adapter", n);

		let ip_packet = IpPacket::try_from(&buf[..n])?;

		let messages = match ip_packet {
			IpPacket::V4(ipv4_packet) => self.handle_ipv4_packet(&ipv4_packet),
			IpPacket::V6(ipv6_packet) => self.handle_ipv6_packet(&ipv6_packet),
		}?;

		tracing::trace!(
			"ip packet handlers gave us {} messages
		",
			messages.len(),
		);

		Ok(messages)
	}

	async fn handle_response(
		&self,
		set: SocketSet,
		response: v2::agent_response::Response,
	) -> Result<(), Self::Error> {
		let (src_port, dst_port) = set.ports();

		let mut reprs = vec![];

		let mut received_data_storage = Vec::new();

		match response {
			// TcpResponse
			v2::agent_response::Response::TcpResponse(tcp_response) => {
				let v2::TcpResponse { response } = tcp_response;

				let Some(response) = response else {
					tracing::warn!("No TCP response found in TcpResponse");
					return Ok(());
				};

				// should always be an existing TCP flow when processing a response
				// mutable by default because we will mostly always change it
				let Some(mut flow) = self.tcp_flows.get_mut(&set) else {
					tracing::warn!("No TCP flow found for set: {:?}", set);
					return Ok(());
				};

				match response {
					// Connected - indicates a connection establishment on agent
					v2::tcp_response::Response::Connected(v2::TcpConnectedResponse {}) => {
						flow.host_current_seq = TcpSeqNumber(rand::random::<i32>());

						let repr =
							tcp_repr_default!(flow, src_port, dst_port, TcpControl::Syn, &[]);

						// update flow as required
						flow.host_current_seq += 1;

						reprs.push(repr);
					}

					// Ok - indicates a successful operation on the agent, or a pure ACK
					v2::tcp_response::Response::Ok(_) => {
						let repr =
							tcp_repr_default!(&flow, src_port, dst_port, TcpControl::None, &[]);

						// update flow as required
						flow.host_current_seq = flow.host_current_seq.add(1);

						reprs.push(repr);
					}

					// DataRecv - indicates data was received on the agent
					v2::tcp_response::Response::DataRecv(res) => {
						received_data_storage = res.data;
						let len = received_data_storage.len();
						let repr = tcp_repr_default!(
							&flow,
							dst_port, // swapping for reply
							src_port,
							TcpControl::None,
							&received_data_storage
						);

						// update flow as required
						flow.host_current_seq = flow.host_current_seq.add(len);

						reprs.push(repr);
					}

					// ConnectionClosed - indicates the connection was closed on the agent
					v2::tcp_response::Response::ConnectionClosed(_) => {
						// ack
						reprs.push(tcp_repr_default!(
							&flow,
							dst_port, // swapping for reply
							src_port,
							TcpControl::None,
							&[]
						));

						// update flow as required
						flow.connection_state = TcpFlowState::FinWait2;

						reprs.push(tcp_repr_default!(
							&flow,
							dst_port, // swapping for reply
							src_port,
							TcpControl::Fin,
							&[]
						));

						// update flow as required
						flow.host_current_seq = flow.host_current_seq.add(1);
					}

					// ConnectionRefused - indicates the connection was refused on agent
					v2::tcp_response::Response::ConnectionRefused(_) => {
						// emit_tcp_segment(&flow, set, TcpControl::Rst, &[], &mut inner_buf)

						let repr = tcp_repr_default!(
							&flow,
							dst_port, // swapping for reply
							src_port,
							TcpControl::Rst,
							&[]
						);

						reprs.push(repr);
					}

					// TODO: Handle other TCP responses
					v2::tcp_response::Response::Listening(_) => todo!("Handle Listening"),
					v2::tcp_response::Response::ListenerClosed(_) => todo!("Handle ListenerClosed"),
					v2::tcp_response::Response::ListenerConnect(_) => {
						// Placeholder, actual handling might differ
						// None
					}
				}
			}

			// IcmpResponse
			v2::agent_response::Response::IcmpResponse(res) => {
				let v2::IcmpResponse { response } = res;

				let Some(v2::icmp_response::Response::DataRecv(v2::IcmpDataRecvResponse {
					echo_ident,
					data,
				})) = response
				else {
					tracing::warn!("No DataRecv ICMP response found in IcmpResponse");
					return Ok(());
				};

				#[allow(clippy::cast_possible_truncation)]
				let key = IcmpFlowHashKey {
					pair: set,
					ident: echo_ident as u16,
				};

				let flow = self
					.icmp_flows
					.entry(key.clone())
					.or_insert_with(|| IcmpFlow { ident: echo_ident });

				received_data_storage = data;

				if let Some(repr) = icmp_repr(&flow, set, &received_data_storage) {
					reprs.push(Repr::Icmp(repr));
				}
			}

			//  UdpResponse
			v2::agent_response::Response::UdpResponse(udp_response) => {
				let Some(_flow) = self.udp_flows.get(&set) else {
					tracing::warn!("No UDP flow found for set: {:?}", set);
					return Ok(());
				};

				let v2::UdpResponse { response } = udp_response;
				let Some(response) = response else {
					tracing::warn!("No response found in UdpResponse");
					return Ok(());
				};

				match response {
					v2::udp_response::Response::DataRecv(res) => {
						received_data_storage = res.data;
						let repr = smoltcp::wire::UdpRepr { src_port, dst_port };
						reprs.push(Repr::Udp(repr));
					}
				}
			}

			// Runtime errors
			v2::agent_response::Response::RuntimeError(res) => {
				tracing::warn!("RuntimeError from agent {:?}", res);
				// None
			}
		};

		let mut packet_buf = [0u8; 1500];

		for repr in reprs {
			let (payload_len, next_header) = match repr {
				Repr::Tcp(repr) => (repr.buffer_len(), IpProtocol::Tcp),
				Repr::Udp(repr) => (repr.header_len(), IpProtocol::Udp),
				Repr::Icmp(repr) => match repr {
					IcmpRepr::Ipv4(repr) => (repr.buffer_len(), IpProtocol::Icmp),
					IcmpRepr::Ipv6(repr) => (repr.buffer_len(), IpProtocol::Icmpv6),
				},
			};

			if payload_len == 0 && next_header != IpProtocol::Tcp {
				// Allow zero payload for TCP ACKs/FINs etc.
				tracing::warn!(
					"No payload to send for outcome, next_header: {:?}",
					next_header
				);
				return Ok(());
			}

			let ip_packet_len = match set {
				SocketSet::Ipv4((src, dst)) => {
					let ipv4_repr = smoltcp::wire::Ipv4Repr {
						src_addr: *dst.ip(), // Swapping for reply
						dst_addr: *src.ip(),
						next_header,
						payload_len,
						hop_limit: 64,
					};

					let ip_header_len = ipv4_repr.buffer_len();
					let ip_packet_len = ip_header_len + payload_len;

					if packet_buf.len() < ip_packet_len {
						tracing::warn!(
							"Packet buffer too small for IPv4. Needed: {}, Available: {}",
							ip_packet_len,
							packet_buf.len()
						);
						return Ok(());
					}

					let mut ip_packet_writer =
						smoltcp::wire::Ipv4Packet::new_unchecked(&mut packet_buf[..ip_header_len]);
					ipv4_repr.emit(&mut ip_packet_writer, &ChecksumCapabilities::default());

					// Copy the payload into the packet buffer
					packet_buf[ip_header_len..ip_packet_len]
						.copy_from_slice(&received_data_storage);

					ip_packet_len
				}
				SocketSet::Ipv6((src, dst)) => {
					let ipv6_repr = smoltcp::wire::Ipv6Repr {
						src_addr: *dst.ip(), // Swapping for reply
						dst_addr: *src.ip(),
						next_header,
						payload_len,
						hop_limit: 64,
					};

					let ip_header_len = ipv6_repr.buffer_len();
					let current_ip_packet_len = ip_header_len + payload_len;

					if packet_buf.len() < current_ip_packet_len {
						tracing::warn!(
							"Packet buffer too small for IPv6. Needed: {}, Available: {}",
							current_ip_packet_len,
							packet_buf.len()
						);
						return Ok(());
					}

					let mut ip_packet_writer =
						smoltcp::wire::Ipv6Packet::new_unchecked(&mut packet_buf[..ip_header_len]);
					ipv6_repr.emit(&mut ip_packet_writer);

					packet_buf[ip_header_len..current_ip_packet_len]
						.copy_from_slice(&received_data_storage);

					current_ip_packet_len
				}
			};

			tracing::trace!(
				"Sending {} bytes: {:02X?}",
				ip_packet_len,
				&packet_buf[..ip_packet_len]
			);

			self.inner.send(&packet_buf[..ip_packet_len]).await?;
		}
		Ok(())
	}
}
