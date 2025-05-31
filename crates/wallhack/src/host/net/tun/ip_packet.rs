use smoltcp::wire::{Ipv4Packet, Ipv6Packet};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("Truncated or empty packet")]
	Truncated,

	#[error("Unknown IP version {0}")]
	UnknownVersion(u8),

	#[error("Parse error {0}")]
	Smoltcp(#[from] smoltcp::wire::Error),
}

#[derive(Debug)]
pub enum IpPacket<'a> {
	V4(Ipv4Packet<&'a [u8]>),
	V6(Ipv6Packet<&'a [u8]>),
}

impl<'buf> TryFrom<&'buf [u8]> for IpPacket<'buf> {
	type Error = Error;

	fn try_from(data: &'buf [u8]) -> Result<Self, Self::Error> {
		if data.is_empty() {
			return Err(Error::Truncated);
		}
		let version = data[0] >> 4;
		match version {
			4 => Ok(IpPacket::V4(Ipv4Packet::new_checked(data)?)),
			6 => Ok(IpPacket::V6(Ipv6Packet::new_checked(data)?)),
			_ => Err(Error::UnknownVersion(version)),
		}
	}
}
