//! Deterministic pcap replay tests targeting Layer A (`InnerStack`).
//!
//! These tests read .pcapng files and replay the captured packets through
//! a [`VecDevice`]-backed [`InnerStack`], asserting that the stack handles
//! diverse real-world traffic without panicking and produces correct
//! responses for TCP handshakes.

use std::fs::File;

use netstack::{
	config::StackConfig,
	inner::{InnerStack, device::VecDevice},
};
use pcap_file::{
	DataLink,
	pcapng::{Block, PcapNgReader},
};
use smoltcp::{
	time::Instant,
	wire::{IpCidr, IpProtocol, Ipv4Address, Ipv4Packet, Ipv6Packet},
};

const PCAP_PATH: &str = "tests/captures/The Ultimate PCAP v20251206.pcapng";

/// Stack IP for tests — we use an address that will appear as a destination
/// in our crafted packets, and won't match most real traffic in the pcap.
const STACK_IPV4: Ipv4Address = Ipv4Address::new(10, 13, 37, 1);

fn test_config() -> StackConfig {
	StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IPV4.into(), 24)],
		random_seed: 0xdead_beef,
		..StackConfig::default()
	}
}

/// Extract the IP-layer payload from a captured frame, stripping the L2 header
/// based on the interface's link type.
///
/// Returns `None` if the link type is unsupported or the frame is too short.
fn strip_l2(linktype: DataLink, data: &[u8]) -> Option<&[u8]> {
	match linktype {
		// Ethernet: 14-byte header (dst[6] + src[6] + ethertype[2])
		DataLink::ETHERNET => {
			if data.len() < 14 {
				return None;
			}
			let ethertype = u16::from_be_bytes([data[12], data[13]]);
			match ethertype {
				0x0800 | 0x86DD => Some(&data[14..]), // IPv4 or IPv6
				0x8100 => {
					// 802.1Q VLAN tag: 4 extra bytes
					if data.len() < 18 {
						return None;
					}
					Some(&data[18..])
				}
				_ => None, // ARP, etc — not IP
			}
		}
		// Raw IP (no L2 header)
		DataLink::RAW | DataLink::IPV4 | DataLink::IPV6 => Some(data),
		// BSD loopback: 4-byte header
		DataLink::NULL => {
			if data.len() < 4 {
				return None;
			}
			Some(&data[4..])
		}
		// Linux cooked capture v1: 16-byte header
		DataLink::LINUX_SLL => {
			if data.len() < 16 {
				return None;
			}
			Some(&data[16..])
		}
		// Linux cooked capture v2: 20-byte header
		DataLink::LINUX_SLL2 => {
			if data.len() < 20 {
				return None;
			}
			Some(&data[20..])
		}
		_ => None,
	}
}

/// Determine if a raw IP payload is IPv4 by checking the version nibble.
fn is_ipv4(ip_data: &[u8]) -> bool {
	!ip_data.is_empty() && (ip_data[0] >> 4) == 4
}

/// Determine if a raw IP payload is IPv6 by checking the version nibble.
fn is_ipv6(ip_data: &[u8]) -> bool {
	!ip_data.is_empty() && (ip_data[0] >> 4) == 6
}

/// Statistics collected during pcap replay.
#[derive(Debug, Default)]
struct ReplayStats {
	total_blocks: usize,
	enhanced_packets: usize,
	ip_packets: usize,
	ipv4_packets: usize,
	ipv6_packets: usize,
	tcp_packets: usize,
	udp_packets: usize,
	icmp_packets: usize,
	injected: usize,
	egress_packets: usize,
	parse_errors: usize,
	unsupported_link: usize,
}

/// Read all enhanced packet blocks from the pcapng file, extract their IP
/// payloads, and feed them into the given stack. Collect statistics.
fn replay_pcap(stack: &mut InnerStack<VecDevice>, path: &str) -> ReplayStats {
	let file = File::open(path).expect("failed to open pcapng file");
	let mut reader = PcapNgReader::new(file).expect("failed to parse pcapng");

	// Build a map of interface_id -> linktype from InterfaceDescription blocks
	let mut iface_linktypes: Vec<DataLink> = Vec::new();

	// Seed from the initial interfaces parsed by PcapNgReader::new()
	for idb in reader.interfaces() {
		iface_linktypes.push(idb.linktype);
	}

	let mut stats = ReplayStats::default();
	let mut timestamp_ms: i64 = 0;

	while let Some(block_result) = reader.next_block() {
		stats.total_blocks += 1;

		let Ok(block) = block_result else {
			stats.parse_errors += 1;
			continue;
		};

		match block {
			Block::InterfaceDescription(idb) => {
				iface_linktypes.push(idb.linktype);
			}
			Block::EnhancedPacket(epb) => {
				stats.enhanced_packets += 1;

				// Advance timestamp monotonically
				let pkt_ts_ms = i64::try_from(epb.timestamp.as_millis()).unwrap_or(i64::MAX);
				if pkt_ts_ms > timestamp_ms {
					timestamp_ms = pkt_ts_ms;
				}

				let linktype = iface_linktypes
					.get(epb.interface_id as usize)
					.copied()
					.unwrap_or(DataLink::ETHERNET);

				let Some(ip_data) = strip_l2(linktype, &epb.data) else {
					stats.unsupported_link += 1;
					continue;
				};

				if ip_data.is_empty() {
					continue;
				}

				if is_ipv4(ip_data) {
					stats.ipv4_packets += 1;
					stats.ip_packets += 1;

					if let Ok(ipv4) = Ipv4Packet::new_checked(ip_data) {
						match ipv4.next_header() {
							IpProtocol::Tcp => stats.tcp_packets += 1,
							IpProtocol::Udp => stats.udp_packets += 1,
							IpProtocol::Icmp => stats.icmp_packets += 1,
							_ => {}
						}
					}
				} else if is_ipv6(ip_data) {
					stats.ipv6_packets += 1;
					stats.ip_packets += 1;

					if let Ok(ipv6) = Ipv6Packet::new_checked(ip_data) {
						match ipv6.next_header() {
							IpProtocol::Tcp => stats.tcp_packets += 1,
							IpProtocol::Udp => stats.udp_packets += 1,
							IpProtocol::Icmpv6 => stats.icmp_packets += 1,
							_ => {}
						}
					}
				} else {
					// Not IPv4 or IPv6 — skip (could be ARP frame body, etc.)
					continue;
				}

				// Inject into the stack
				stack.device_mut().inject(ip_data.to_vec());
				stats.injected += 1;

				// Poll after each packet for deterministic state transitions
				let now = Instant::from_millis(timestamp_ms);
				stack.poll(now);

				// Drain egress to count responses
				let egress = stack.device_mut().drain_egress();
				stats.egress_packets += egress.len();
			}
			_ => {}
		}
	}

	// Final poll to flush any pending state
	let now = Instant::from_millis(timestamp_ms + 1);
	stack.poll(now);
	let egress = stack.device_mut().drain_egress();
	stats.egress_packets += egress.len();

	stats
}

/// Robustness test: Feed every IP packet from the Ultimate PCAP into the stack.
///
/// The stack must not panic on any packet, regardless of protocol or malformed data.
/// This is the core "fuzz-like" deterministic test the NETSTACK spec requires.
#[test]
fn pcap_replay_no_panic() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	// Listen on a few common ports so the stack has active sockets
	let _h80 = stack.tcp_listen(80).expect("listen on 80");
	let _h443 = stack.tcp_listen(443).expect("listen on 443");
	let _h22 = stack.tcp_listen(22).expect("listen on 22");

	let stats = replay_pcap(&mut stack, PCAP_PATH);

	eprintln!("=== PCAP Replay Stats ===");
	eprintln!("  Total blocks:      {}", stats.total_blocks);
	eprintln!("  Enhanced packets:  {}", stats.enhanced_packets);
	eprintln!("  IP packets:        {}", stats.ip_packets);
	eprintln!("    IPv4:            {}", stats.ipv4_packets);
	eprintln!("    IPv6:            {}", stats.ipv6_packets);
	eprintln!("    TCP:             {}", stats.tcp_packets);
	eprintln!("    UDP:             {}", stats.udp_packets);
	eprintln!("    ICMP:            {}", stats.icmp_packets);
	eprintln!("  Injected:          {}", stats.injected);
	eprintln!("  Egress packets:    {}", stats.egress_packets);
	eprintln!("  Parse errors:      {}", stats.parse_errors);
	eprintln!("  Unsupported link:  {}", stats.unsupported_link);

	// Sanity: the file should contain a meaningful number of packets
	assert!(
		stats.enhanced_packets > 100,
		"expected >100 enhanced packets, got {}",
		stats.enhanced_packets
	);
	assert!(
		stats.ip_packets > 50,
		"expected >50 IP packets, got {}",
		stats.ip_packets
	);
	// We should have injected packets without crashing
	assert!(
		stats.injected > 50,
		"expected >50 injected packets, got {}",
		stats.injected
	);
}

/// Test that injecting a crafted TCP SYN addressed to the stack's IP
/// via the pcap replay pathway produces a SYN-ACK, verifying the full
/// pipeline: device inject → poll → egress capture.
#[test]
fn pcap_replay_targeted_syn() {
	use smoltcp::wire::{IpProtocol, Ipv4Packet, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber};

	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());
	let _h = stack.tcp_listen(8080).expect("listen on 8080");

	// Build SYN: external_ip:54321 → STACK_IPV4:8080
	let tcp_repr = TcpRepr {
		src_port: 54321,
		dst_port: 8080,
		control: TcpControl::Syn,
		seq_number: TcpSeqNumber(5000),
		ack_number: None,
		window_len: 65535,
		window_scale: Some(7),
		max_seg_size: Some(1460),
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	let now = Instant::from_millis(1000);
	let egress = inject_tcp_packet(&mut stack, Ipv4Address::new(10, 13, 37, 2), &tcp_repr, now);
	assert!(!egress.is_empty(), "expected SYN-ACK response");

	let reply_ip = Ipv4Packet::new_checked(&egress[0]).expect("valid IPv4");
	assert_eq!(reply_ip.src_addr(), STACK_IPV4);
	assert_eq!(reply_ip.dst_addr(), Ipv4Address::new(10, 13, 37, 2));
	assert_eq!(reply_ip.next_header(), IpProtocol::Tcp);

	let reply_tcp = TcpPacket::new_checked(reply_ip.payload()).expect("valid TCP");
	assert!(reply_tcp.syn(), "SYN flag");
	assert!(reply_tcp.ack(), "ACK flag");
	assert_eq!(reply_tcp.src_port(), 8080);
	assert_eq!(reply_tcp.dst_port(), 54321);
	assert_eq!(reply_tcp.ack_number(), TcpSeqNumber(5001));
}

/// Build and inject a TCP packet from `src_addr` → `STACK_IPV4` into the stack,
/// poll it, and return egress packets.
fn inject_tcp_packet(
	stack: &mut InnerStack<VecDevice>,
	src_addr: Ipv4Address,
	tcp_repr: &smoltcp::wire::TcpRepr<'_>,
	now: Instant,
) -> Vec<Vec<u8>> {
	use smoltcp::wire::{IpProtocol, Ipv4Packet, Ipv4Repr};

	let ip_repr = Ipv4Repr {
		src_addr,
		dst_addr: STACK_IPV4,
		next_header: IpProtocol::Tcp,
		payload_len: tcp_repr.header_len() + tcp_repr.payload.len(),
		hop_limit: 64,
	};
	let total_len = ip_repr.buffer_len() + tcp_repr.header_len() + tcp_repr.payload.len();
	let mut buf = vec![0u8; total_len];
	{
		let mut ip_pkt = Ipv4Packet::new_unchecked(&mut buf);
		ip_repr.emit(&mut ip_pkt, &smoltcp::phy::ChecksumCapabilities::default());
	}
	let ip_hdr_len = {
		let ip_pkt = Ipv4Packet::new_unchecked(&buf);
		ip_pkt.header_len() as usize
	};
	{
		let tcp_with_payload = smoltcp::wire::TcpRepr {
			payload: tcp_repr.payload,
			..*tcp_repr
		};
		let mut tcp_pkt = smoltcp::wire::TcpPacket::new_unchecked(&mut buf[ip_hdr_len..]);
		tcp_with_payload.emit(
			&mut tcp_pkt,
			&src_addr.into(),
			&STACK_IPV4.into(),
			&smoltcp::phy::ChecksumCapabilities::default(),
		);
	}
	stack.device_mut().inject(buf);
	stack.poll(now);
	stack.device_mut().drain_egress()
}

/// Test a complete TCP three-way handshake via deterministic replay:
/// SYN → SYN-ACK → ACK → ESTABLISHED.
#[test]
fn pcap_replay_full_handshake() {
	use smoltcp::{
		socket::tcp::State,
		wire::{Ipv4Packet, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber},
	};

	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());
	let handle = stack.tcp_listen(9000).expect("listen on 9000");

	let client_ip = Ipv4Address::new(10, 13, 37, 100);
	let client_port: u16 = 40_000;

	// Step 1: SYN
	let client_isn = TcpSeqNumber(10_000);
	let syn = TcpRepr {
		src_port: client_port,
		dst_port: 9000,
		control: TcpControl::Syn,
		seq_number: client_isn,
		ack_number: None,
		window_len: 65535,
		window_scale: Some(7),
		max_seg_size: Some(1460),
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	let now = Instant::from_millis(100);
	let egress = inject_tcp_packet(&mut stack, client_ip, &syn, now);
	assert!(!egress.is_empty(), "expected SYN-ACK");

	// Parse SYN-ACK to get server ISN
	let sa_ip = Ipv4Packet::new_checked(&egress[0]).expect("valid IPv4");
	let sa_tcp = TcpPacket::new_checked(sa_ip.payload()).expect("valid TCP");
	assert!(sa_tcp.syn() && sa_tcp.ack(), "expected SYN-ACK flags");
	let server_isn = TcpSeqNumber(sa_tcp.seq_number().0);

	// Socket should be in SYN-RECEIVED
	assert_eq!(stack.tcp_socket(handle).state(), State::SynReceived);

	// Step 2: ACK (completes handshake)
	let ack = TcpRepr {
		src_port: client_port,
		dst_port: 9000,
		control: TcpControl::None,
		seq_number: TcpSeqNumber(client_isn.0 + 1),
		ack_number: Some(TcpSeqNumber(server_isn.0 + 1)),
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	let now = Instant::from_millis(200);
	let _egress = inject_tcp_packet(&mut stack, client_ip, &ack, now);

	// Socket should now be ESTABLISHED
	assert_eq!(stack.tcp_socket(handle).state(), State::Established);

	// Step 3: Send some data from client
	let data = b"Hello, netstack!";
	let data_pkt = TcpRepr {
		src_port: client_port,
		dst_port: 9000,
		control: TcpControl::Psh,
		seq_number: TcpSeqNumber(client_isn.0 + 1),
		ack_number: Some(TcpSeqNumber(server_isn.0 + 1)),
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: data,
		timestamp: None,
	};

	let now = Instant::from_millis(300);
	let _egress = inject_tcp_packet(&mut stack, client_ip, &data_pkt, now);

	// The stack should have received the data
	let socket = stack.tcp_socket_mut(handle);
	let mut recv_buf = [0u8; 64];
	let n = socket.recv_slice(&mut recv_buf).expect("recv data");
	assert_eq!(&recv_buf[..n], data);
}
