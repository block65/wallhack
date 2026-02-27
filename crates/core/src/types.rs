use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use wallhack_wire::control::NodeRole as ProtoNodeRole;

/// Node role for configuration and identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Entry,
    Relay,
    Exit,
}

impl From<NodeRole> for ProtoNodeRole {
    fn from(role: NodeRole) -> Self {
        match role {
            NodeRole::Entry => ProtoNodeRole::RoleEntry,
            NodeRole::Relay => ProtoNodeRole::RoleRelay,
            NodeRole::Exit => ProtoNodeRole::RoleExit,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NodeRoleError {
    #[error("node role is unset")]
    Unset,
}

impl TryFrom<ProtoNodeRole> for NodeRole {
    type Error = NodeRoleError;

    fn try_from(role: ProtoNodeRole) -> Result<Self, Self::Error> {
        match role {
            ProtoNodeRole::RoleEntry => Ok(NodeRole::Entry),
            ProtoNodeRole::RoleRelay => Ok(NodeRole::Relay),
            ProtoNodeRole::RoleExit => Ok(NodeRole::Exit),
            ProtoNodeRole::RoleUnknown => Err(NodeRoleError::Unset),
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeRole::Entry => write!(f, "entry"),
            NodeRole::Relay => write!(f, "relay"),
            NodeRole::Exit => write!(f, "exit"),
        }
    }
}

/// A parsed CIDR notation (e.g. `10.0.0.0/8`).
///
/// Parse-don't-validate: constructing a [`Cidr`] guarantees the address and
/// prefix length are valid for the IP version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    addr: IpAddr,
    prefix_len: u8,
}

/// Errors that can occur when parsing a CIDR string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CidrParseError {
    #[error("missing '/' separator in CIDR notation")]
    MissingSeparator,

    #[error("invalid IP address: {0}")]
    InvalidAddr(#[from] std::net::AddrParseError),

    #[error("invalid prefix length: {0}")]
    InvalidPrefixLen(#[from] std::num::ParseIntError),

    #[error("prefix length {prefix_len} exceeds maximum {max} for IP version")]
    PrefixLenTooLarge { prefix_len: u8, max: u8 },
}

impl Cidr {
    /// Returns the IP address component.
    #[must_use]
    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    /// Returns the prefix length.
    #[must_use]
    pub fn prefix_len(&self) -> u8 {
        self.prefix_len
    }
}

impl FromStr for Cidr {
    type Err = CidrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr_str, prefix_str) = s.split_once('/').ok_or(CidrParseError::MissingSeparator)?;

        let addr: IpAddr = addr_str.parse()?;
        let prefix_len: u8 = prefix_str.parse()?;

        let max = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };

        if prefix_len > max {
            return Err(CidrParseError::PrefixLenTooLarge { prefix_len, max });
        }

        Ok(Self { addr, prefix_len })
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix_len)
    }
}

/// Normalize an IPv4-mapped IPv6 socket address to plain IPv4.
///
/// Dual-stack sockets present IPv4 peers as `[::ffff:x.x.x.x]:port`.
/// This converts them to `x.x.x.x:port` for cleaner display and consistent
/// matching. Pure IPv6 addresses are returned unchanged.
#[must_use]
pub fn normalize_socket_addr(addr: SocketAddr) -> SocketAddr {
    if let SocketAddr::V6(v6) = addr
        && let Some(ipv4) = v6.ip().to_ipv4_mapped()
    {
        return SocketAddr::new(IpAddr::V4(ipv4), v6.port());
    }
    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cidr_parse_ipv4() {
        let cidr: Cidr = "10.0.0.0/8".parse().unwrap();
        assert_eq!(cidr.addr(), IpAddr::from([10, 0, 0, 0]));
        assert_eq!(cidr.prefix_len(), 8);
        assert_eq!(cidr.to_string(), "10.0.0.0/8");
    }

    #[test]
    fn test_cidr_parse_ipv6() {
        let cidr: Cidr = "fe80::/10".parse().unwrap();
        assert_eq!(cidr.prefix_len(), 10);
        assert_eq!(cidr.to_string(), "fe80::/10");
    }

    #[test]
    fn test_cidr_parse_max_prefix() {
        assert!("0.0.0.0/0".parse::<Cidr>().is_ok());
        assert!("255.255.255.255/32".parse::<Cidr>().is_ok());
        assert!("::/0".parse::<Cidr>().is_ok());
        assert!("::1/128".parse::<Cidr>().is_ok());
    }

    #[test]
    fn test_cidr_parse_errors() {
        assert!(matches!(
            "10.0.0.0".parse::<Cidr>(),
            Err(CidrParseError::MissingSeparator)
        ));
        assert!(matches!(
            "not-an-ip/8".parse::<Cidr>(),
            Err(CidrParseError::InvalidAddr(_))
        ));
        assert!(matches!(
            "10.0.0.0/abc".parse::<Cidr>(),
            Err(CidrParseError::InvalidPrefixLen(_))
        ));
        assert!(matches!(
            "10.0.0.0/33".parse::<Cidr>(),
            Err(CidrParseError::PrefixLenTooLarge {
                prefix_len: 33,
                max: 32
            })
        ));
        assert!(matches!(
            "::1/129".parse::<Cidr>(),
            Err(CidrParseError::PrefixLenTooLarge {
                prefix_len: 129,
                max: 128
            })
        ));
    }

    #[test]
    fn normalize_ipv4_mapped_ipv6() {
        use std::net::{Ipv6Addr, SocketAddrV6};

        // IPv4-mapped IPv6 -> plain IPv4
        let mapped = SocketAddr::V6(SocketAddrV6::new(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap(),
            6565,
            0,
            0,
        ));
        let normalized = normalize_socket_addr(mapped);
        assert_eq!(normalized.to_string(), "127.0.0.1:6565");

        // Pure IPv6 unchanged
        let v6 = SocketAddr::V6(SocketAddrV6::new(
            "::1".parse::<Ipv6Addr>().unwrap(),
            6565,
            0,
            0,
        ));
        assert_eq!(normalize_socket_addr(v6), v6);

        // Plain IPv4 unchanged
        let v4: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        assert_eq!(normalize_socket_addr(v4), v4);
    }

    #[test]
    fn test_cidr_equality_and_hash() {
        use std::collections::HashSet;

        let a: Cidr = "10.0.0.0/8".parse().unwrap();
        let b: Cidr = "10.0.0.0/8".parse().unwrap();
        let c: Cidr = "192.168.0.0/16".parse().unwrap();

        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }
}
