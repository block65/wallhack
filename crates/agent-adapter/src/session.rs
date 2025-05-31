use crate::sessions;

#[derive(Debug)]
pub enum Session {
	Tcp(sessions::tcp::TcpSession),
	Udp(sessions::udp::UdpSession),
	Icmp(sessions::icmp::IcmpSession),
}
