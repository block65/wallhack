use std::time::Duration;

use smoltcp::wire::{
	IpCidr, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, TcpControl, TcpPacket, TcpRepr,
	TcpSeqNumber, UdpPacket, UdpRepr,
};
use tokio::time::Instant;

use crate::{config::StackConfig, inner::device::VecDevice};

use super::Netstack;

// ============================================================================
// Constants
// ============================================================================

pub const STACK_IP: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);
pub const CLIENT_IP: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);
pub const CLIENT_SRC_PORT: u16 = 12345;
pub const CLIENT_ISN: i32 = 1000;

// ============================================================================
// Config helper
// ============================================================================

pub fn test_config() -> StackConfig {
	StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		random_seed: 42,
		..StackConfig::default()
	}
}

// ============================================================================
// Packet builders
// ============================================================================

pub fn create_syn_packet(dst_port: u16) -> Vec<u8> {
	create_syn_packet_from(CLIENT_SRC_PORT, dst_port, TcpSeqNumber(CLIENT_ISN))
}

pub fn create_syn_packet_from(src_port: u16, dst_port: u16, seq: TcpSeqNumber) -> Vec<u8> {
	let tcp_repr = TcpRepr {
		src_port,
		dst_port,
		control: TcpControl::Syn,
		seq_number: seq,
		ack_number: None,
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	emit_tcp_packet(&tcp_repr)
}

pub fn create_ack_packet(dst_port: u16, seq: TcpSeqNumber, ack: TcpSeqNumber) -> Vec<u8> {
	create_ack_packet_from(CLIENT_SRC_PORT, dst_port, seq, ack)
}

pub fn create_ack_packet_from(
	src_port: u16,
	dst_port: u16,
	seq: TcpSeqNumber,
	ack: TcpSeqNumber,
) -> Vec<u8> {
	let tcp_repr = TcpRepr {
		src_port,
		dst_port,
		control: TcpControl::None,
		seq_number: seq,
		ack_number: Some(ack),
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	emit_tcp_packet(&tcp_repr)
}

pub fn create_data_packet(
	dst_port: u16,
	seq: TcpSeqNumber,
	ack: TcpSeqNumber,
	data: &[u8],
) -> Vec<u8> {
	create_data_packet_from(CLIENT_SRC_PORT, dst_port, seq, ack, data)
}

pub fn create_data_packet_from(
	src_port: u16,
	dst_port: u16,
	seq: TcpSeqNumber,
	ack: TcpSeqNumber,
	data: &[u8],
) -> Vec<u8> {
	let tcp_repr = TcpRepr {
		src_port,
		dst_port,
		control: TcpControl::Psh,
		seq_number: seq,
		ack_number: Some(ack),
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: data,
		timestamp: None,
	};

	emit_tcp_packet(&tcp_repr)
}

pub fn create_fin_packet(dst_port: u16, seq: TcpSeqNumber, ack: TcpSeqNumber) -> Vec<u8> {
	create_fin_packet_from(CLIENT_SRC_PORT, dst_port, seq, ack)
}

pub fn create_fin_packet_from(
	src_port: u16,
	dst_port: u16,
	seq: TcpSeqNumber,
	ack: TcpSeqNumber,
) -> Vec<u8> {
	let tcp_repr = TcpRepr {
		src_port,
		dst_port,
		control: TcpControl::Fin,
		seq_number: seq,
		ack_number: Some(ack),
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};

	emit_tcp_packet(&tcp_repr)
}

fn emit_tcp_packet(tcp_repr: &TcpRepr<'_>) -> Vec<u8> {
	let ip_repr = Ipv4Repr {
		src_addr: CLIENT_IP,
		dst_addr: STACK_IP,
		next_header: IpProtocol::Tcp,
		payload_len: tcp_repr.header_len() + tcp_repr.payload.len(),
		hop_limit: 64,
	};

	let total_len = ip_repr.buffer_len() + tcp_repr.header_len() + tcp_repr.payload.len();
	let mut packet_buf = vec![0u8; total_len];
	let mut ipv4_pkt = Ipv4Packet::new_unchecked(&mut packet_buf);
	ip_repr.emit(
		&mut ipv4_pkt,
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	let mut tcp_pkt = TcpPacket::new_unchecked(ipv4_pkt.payload_mut());
	tcp_repr.emit(
		&mut tcp_pkt,
		&CLIENT_IP.into(),
		&STACK_IP.into(),
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	packet_buf
}

pub fn create_udp_packet(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
	let udp_repr = UdpRepr { src_port, dst_port };

	let ip_repr = Ipv4Repr {
		src_addr: CLIENT_IP,
		dst_addr: STACK_IP,
		next_header: IpProtocol::Udp,
		payload_len: udp_repr.header_len() + data.len(),
		hop_limit: 64,
	};

	let total_len = ip_repr.buffer_len() + udp_repr.header_len() + data.len();
	let mut packet_buf = vec![0u8; total_len];
	let mut ipv4_pkt = Ipv4Packet::new_unchecked(&mut packet_buf);
	ip_repr.emit(
		&mut ipv4_pkt,
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	let mut udp_pkt = UdpPacket::new_unchecked(ipv4_pkt.payload_mut());
	udp_repr.emit(
		&mut udp_pkt,
		&CLIENT_IP.into(),
		&STACK_IP.into(),
		data.len(),
		|buf| buf.copy_from_slice(data),
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	packet_buf
}

// ============================================================================
// Handshake helpers
// ============================================================================

#[allow(dead_code)]
pub struct HandshakeResult {
	pub server_isn: TcpSeqNumber,
	pub client_next_seq: TcpSeqNumber,
	pub server_next_seq: TcpSeqNumber,
}

/// Complete a TCP 3-way handshake with the stack using default client `src_port`/ISN.
pub async fn complete_handshake(stack: &Netstack<VecDevice>, port: u16) -> HandshakeResult {
	complete_handshake_from(stack, CLIENT_SRC_PORT, port, TcpSeqNumber(CLIENT_ISN)).await
}

/// Complete a TCP 3-way handshake with parameterized source port and sequence.
pub async fn complete_handshake_from(
	stack: &Netstack<VecDevice>,
	src_port: u16,
	dst_port: u16,
	seq: TcpSeqNumber,
) -> HandshakeResult {
	// Inject SYN
	{
		let mut inner = stack.shared.inner.lock();
		inner
			.device_mut()
			.inject(create_syn_packet_from(src_port, dst_port, seq));
	}
	stack.wake();

	// Wait for SYN-ACK
	let start = Instant::now();
	loop {
		let maybe_server_seq = {
			let mut inner = stack.shared.inner.lock();
			let egress = inner.device_mut().drain_egress();
			let mut found = None;
			for pkt in &egress {
				if let Ok(ip_pkt) = Ipv4Packet::new_checked(pkt.as_slice())
					&& let Ok(tcp_pkt) = TcpPacket::new_checked(ip_pkt.payload())
					&& tcp_pkt.syn()
					&& tcp_pkt.ack()
					&& tcp_pkt.dst_port() == src_port
					&& tcp_pkt.src_port() == dst_port
				{
					found = Some(tcp_pkt.seq_number());
					break;
				}
			}
			found
		};

		if let Some(server_seq) = maybe_server_seq {
			// Send ACK to complete handshake
			let client_next = seq + 1;
			let server_next = server_seq + 1;
			{
				let mut inner2 = stack.shared.inner.lock();
				inner2.device_mut().inject(create_ack_packet_from(
					src_port,
					dst_port,
					client_next,
					server_next,
				));
			}
			stack.wake();

			// Give the poll loop a chance to process
			tokio::task::yield_now().await;

			return HandshakeResult {
				server_isn: server_seq,
				client_next_seq: client_next,
				server_next_seq: server_next,
			};
		}

		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout waiting for SYN-ACK on port {dst_port} from src_port {src_port}",
		);
		tokio::task::yield_now().await;
	}
}
