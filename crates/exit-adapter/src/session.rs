use crate::sessions;

#[derive(Debug, Clone)]
pub enum Session {
    Tcp(sessions::tcp::TcpSession),
    Udp(sessions::udp::UdpSession),
    #[cfg(unix)]
    Icmp(sessions::icmp::IcmpSession),
}
