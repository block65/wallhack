use protobuf::SocketSet;
use smoltcp::{
	phy::ChecksumCapabilities,
	wire::{IpAddress, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber},
};

#[derive(Debug, Clone, PartialEq)]
pub enum TcpFlowState {
	None,
	SynReceived,
	Established,
	Listen,
	// Close related
	FinWait1,
	FinWait2,
	TimeWait,
	// CloseWait,
	// Closing,
	// LastAck,
}

#[derive(Debug, Clone)]
pub struct TcpFlow {
	// Sequence number of the next byte the Host expects to receive from the Client.
	// Initialized to client_isn + 1 after receiving SYN. Incremented by data received from Client.
	pub ack_for_client_seq: TcpSeqNumber,

	// Sequence number of the next byte the Host will send to the Client.
	// Initialized to host_isn + 1 after sending SYN-ACK. Incremented by data (and SYN/FIN) sent by Host.
	pub host_current_seq: TcpSeqNumber,

	// Last advertised window by the client. Dictates how much data Host can send.
	pub client_advertised_window: u16,

	// Host's own receive window it advertises to the client.
	// Based on Host's buffer availability for incoming data from Client.
	pub host_advertised_window: u16,

	// TCP connection state (e.g., Established, FinWait1, Closing, etc.)
	// This is a simplified representation. A full TCP state machine is more complex.
	pub connection_state: TcpFlowState,
}

impl Default for TcpFlow {
	fn default() -> Self {
		TcpFlow {
			connection_state: TcpFlowState::None,
			ack_for_client_seq: TcpSeqNumber(0),
			host_current_seq: TcpSeqNumber(0),
			host_advertised_window: 65535,
			client_advertised_window: 65535,
		}
	}
}

pub type TcpFlowHashKey = SocketSet;

pub enum BuildOutcome {
	Icmp(usize),
	Tcp(usize),
	Udp(usize),
}

// Helper function to build and emit a TCP segment into a packet buffer.
pub fn emit_tcp_segment(
	flow: &TcpFlow,
	socket_set: SocketSet,
	control: TcpControl,
	payload: &[u8],
	packet_buf: &mut [u8],
) -> Option<BuildOutcome> {
	// break down the socket_set into source and destination IPs and ports
	let (src_ip, dst_ip, src_port, dst_port) = match socket_set {
		SocketSet::Ipv4((s, d)) => (
			IpAddress::Ipv4(*s.ip()),
			IpAddress::Ipv4(*d.ip()),
			s.port(),
			d.port(),
		),
		SocketSet::Ipv6((s, d)) => (
			IpAddress::Ipv6(*s.ip()),
			IpAddress::Ipv6(*d.ip()),
			s.port(),
			d.port(),
		),
	};

	let tcp_repr = TcpRepr {
		control,
		seq_number: flow.host_current_seq,
		ack_number: Some(flow.ack_for_client_seq),
		window_len: flow.host_advertised_window,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload,
		src_port: dst_port, // Swapping for reply
		dst_port: src_port,
		timestamp: None,
	};

	let tcp_segment_len = tcp_repr.buffer_len(); // This is TCP header + TCP payload length

	if packet_buf.len() < tcp_segment_len {
		tracing::error!(
			"Packet buffer too small for TCP segment. Needed: {}, Available: {}",
			tcp_segment_len,
			packet_buf.len()
		);
		return None;
	}

	let mut tcp_packet = TcpPacket::new_unchecked(&mut packet_buf[..tcp_segment_len]);
	tcp_repr.emit(
		&mut tcp_packet,
		&dst_ip, // Swapping for reply
		&src_ip,
		&ChecksumCapabilities::default(),
	);

	Some(BuildOutcome::Tcp(tcp_segment_len))
}
