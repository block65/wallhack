use std::fmt::Display;

use crate::{data, helpers::ConversionError};

#[derive(Hash, Eq, PartialEq, Debug, Copy, Clone)]
pub enum SocketSet {
    Ipv4((std::net::SocketAddrV4, std::net::SocketAddrV4)),
    Ipv6((std::net::SocketAddrV6, std::net::SocketAddrV6)),
}

impl SocketSet {
    #[must_use]
    pub fn ports(self) -> (u16, u16) {
        match self {
            SocketSet::Ipv4((src, dst)) => (src.port(), dst.port()),
            SocketSet::Ipv6((src, dst)) => (src.port(), dst.port()),
        }
    }
}

impl Display for SocketSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocketSet::Ipv4((src, dst)) => write!(f, "{src}-{dst}"),
            SocketSet::Ipv6((src, dst)) => write!(f, "{src}-{dst}"),
        }
    }
}

impl TryFrom<(data::SocketV4Address, data::SocketV4Address)> for SocketSet {
    type Error = ConversionError;

    fn try_from(
        (src, dst): (data::SocketV4Address, data::SocketV4Address),
    ) -> std::result::Result<Self, Self::Error> {
        if let Some(pair) = src.ip.zip(dst.ip) {
            let (src_ip, dst_ip) = pair;
            let src_port = u16::try_from(src.port).map_err(|_| ConversionError::InvalidPort)?;
            let dst_port = u16::try_from(dst.port).map_err(|_| ConversionError::InvalidPort)?;
            Ok(SocketSet::Ipv4((
                std::net::SocketAddrV4::new(src_ip.try_into()?, src_port),
                std::net::SocketAddrV4::new(dst_ip.try_into()?, dst_port),
            )))
        } else {
            Err(Self::Error::InvalidSocketAddrPair)
        }
    }
}

impl From<SocketSet> for data::SocketAddressPair {
    fn from(val: SocketSet) -> Self {
        match val {
            SocketSet::Ipv4((src, dst)) => data::SocketAddressPair {
                pair: Some(data::socket_address_pair::Pair::Ipv4(
                    data::SocketV4AddressPair {
                        src_addr: Some(data::SocketV4Address {
                            ip: Some(data::IpV4Address {
                                ip: src.ip().octets().to_vec(),
                            }),
                            port: u32::from(src.port()),
                        }),
                        dst_addr: Some(data::SocketV4Address {
                            ip: Some(data::IpV4Address {
                                ip: dst.ip().octets().to_vec(),
                            }),
                            port: u32::from(dst.port()),
                        }),
                    },
                )),
            },
            SocketSet::Ipv6((src, dst)) => data::SocketAddressPair {
                pair: Some(data::socket_address_pair::Pair::Ipv6(
                    data::SocketV6AddressPair {
                        src_addr: Some(data::SocketV6Address {
                            ip: Some(data::IpV6Address {
                                ip: src.ip().octets().to_vec(),
                            }),
                            port: u32::from(src.port()),
                            flowinfo: src.flowinfo(),
                            scope_id: src.scope_id(),
                        }),
                        dst_addr: Some(data::SocketV6Address {
                            ip: Some(data::IpV6Address {
                                ip: dst.ip().octets().to_vec(),
                            }),
                            port: u32::from(dst.port()),
                            flowinfo: dst.flowinfo(),
                            scope_id: dst.scope_id(),
                        }),
                    },
                )),
            },
        }
    }
}

impl TryFrom<(data::SocketV6Address, data::SocketV6Address)> for SocketSet {
    type Error = ConversionError;

    fn try_from(
        (src, dst): (data::SocketV6Address, data::SocketV6Address),
    ) -> std::result::Result<Self, Self::Error> {
        if let Some(pair) = src.ip.zip(dst.ip) {
            let (src_ip, dst_ip) = pair;
            let src_port = u16::try_from(src.port).map_err(|_| ConversionError::InvalidPort)?;
            let dst_port = u16::try_from(dst.port).map_err(|_| ConversionError::InvalidPort)?;
            Ok(SocketSet::Ipv6((
                std::net::SocketAddrV6::new(
                    src_ip.try_into()?,
                    src_port,
                    src.flowinfo,
                    src.scope_id,
                ),
                std::net::SocketAddrV6::new(
                    dst_ip.try_into()?,
                    dst_port,
                    dst.flowinfo,
                    dst.scope_id,
                ),
            )))
        } else {
            Err(Self::Error::InvalidSocketAddrPair)
        }
    }
}

impl TryFrom<data::socket_address_pair::Pair> for SocketSet {
    type Error = ConversionError;

    fn try_from(pair: data::socket_address_pair::Pair) -> std::result::Result<Self, Self::Error> {
        match pair {
            data::socket_address_pair::Pair::Ipv4(pair) => {
                let maybe_pair = pair.src_addr.zip(pair.dst_addr);
                let Some(pair) = maybe_pair else {
                    return Err(Self::Error::InvalidSocketAddrPair);
                };

                SocketSet::try_from(pair)
            }
            data::socket_address_pair::Pair::Ipv6(pair) => {
                let maybe_pair = pair.src_addr.zip(pair.dst_addr);
                let Some(pair) = maybe_pair else {
                    return Err(Self::Error::InvalidSocketAddrPair);
                };

                SocketSet::try_from(pair)
            }
        }
    }
}

impl From<SocketSet> for (std::net::SocketAddr, std::net::SocketAddr) {
    fn from(val: SocketSet) -> Self {
        match val {
            SocketSet::Ipv4((src, dst)) => {
                (std::net::SocketAddr::V4(src), std::net::SocketAddr::V4(dst))
            }
            SocketSet::Ipv6((src, dst)) => {
                (std::net::SocketAddr::V6(src), std::net::SocketAddr::V6(dst))
            }
        }
    }
}

impl TryFrom<data::SocketV4AddressPair> for SocketSet {
    type Error = ConversionError;

    fn try_from(pair: data::SocketV4AddressPair) -> std::result::Result<Self, Self::Error> {
        let maybe_pair = pair.src_addr.zip(pair.dst_addr);
        let Some(pair) = maybe_pair else {
            return Err(Self::Error::InvalidSocketAddrPair);
        };

        SocketSet::try_from(pair)
    }
}

impl TryFrom<data::SocketV6AddressPair> for SocketSet {
    type Error = ConversionError;

    fn try_from(pair: data::SocketV6AddressPair) -> std::result::Result<Self, Self::Error> {
        let maybe_pair = pair.src_addr.zip(pair.dst_addr);
        let Some(pair) = maybe_pair else {
            return Err(Self::Error::InvalidSocketAddrPair);
        };

        SocketSet::try_from(pair)
    }
}

impl TryFrom<data::SocketAddressPair> for SocketSet {
    type Error = ConversionError;

    fn try_from(s: data::SocketAddressPair) -> Result<Self, Self::Error> {
        match s.pair {
            Some(data::socket_address_pair::Pair::Ipv4(pair)) => SocketSet::try_from(pair),
            Some(data::socket_address_pair::Pair::Ipv6(pair)) => SocketSet::try_from(pair),
            _ => Err(Self::Error::InvalidSocketAddrPair),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_port_rejected() {
        let addr = data::SocketV4Address {
            ip: Some(data::IpV4Address {
                ip: vec![127, 0, 0, 1],
            }),
            port: 70_000,
        };
        let result = std::net::SocketAddrV4::try_from(addr);
        assert!(matches!(result, Err(ConversionError::InvalidPort)));
    }
}
