//! Netlink helpers: OS route management and local address enumeration.

use std::{net::IpAddr, str::FromStr};

use neli::{
    consts::{
        nl::NlmF,
        rtnl::{Ifa, IfaF, RtAddrFamily, RtScope, RtTable, Rta, Rtm, RtmF, Rtn, Rtprot},
        socket::NlFamily,
    },
    nl::{NlPayload, NlmsghdrBuilder},
    rtnl::{Ifaddrmsg, IfaddrmsgBuilder, RtattrBuilder, RtmsgBuilder},
    socket::synchronous::NlSocketHandle,
    types::RtBuffer,
    utils::Groups,
};

use wallhack_core::Cidr;

pub(crate) fn get_if_index(name: &str) -> std::io::Result<u32> {
    let path = format!("/sys/class/net/{name}/ifindex");
    let content = std::fs::read_to_string(path)?;
    content
        .trim()
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Remove an OS-level route via Netlink.
pub(crate) fn remove_os_route(cidr: &str, dev: &str) -> Result<(), String> {
    let cidr = Cidr::from_str(cidr).map_err(|e| e.to_string())?;
    let if_index =
        get_if_index(dev).map_err(|e| format!("Failed to resolve interface {dev}: {e}"))?;

    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, Groups::empty())
        .map_err(|e| format!("Netlink connect failed: {e}"))?;

    let (rt_family, dst_bytes) = match cidr.addr() {
        std::net::IpAddr::V4(addr) => (RtAddrFamily::Inet, addr.octets().to_vec()),
        std::net::IpAddr::V6(addr) => (RtAddrFamily::Inet6, addr.octets().to_vec()),
    };

    let mut rtattrs = RtBuffer::new();
    rtattrs.push(
        RtattrBuilder::default()
            .rta_type(Rta::Dst)
            .rta_payload(dst_bytes)
            .build()
            .unwrap(),
    );
    #[allow(clippy::cast_possible_wrap)]
    rtattrs.push(
        RtattrBuilder::default()
            .rta_type(Rta::Oif)
            .rta_payload(if_index as i32)
            .build()
            .unwrap(),
    );

    let rtmsg = RtmsgBuilder::default()
        .rtm_family(rt_family)
        .rtm_dst_len(cidr.prefix_len())
        .rtm_src_len(0)
        .rtm_tos(0)
        .rtm_table(RtTable::Main)
        .rtm_protocol(Rtprot::Boot)
        .rtm_scope(RtScope::Universe)
        .rtm_type(Rtn::Unicast)
        .rtm_flags(RtmF::empty())
        .rtattrs(rtattrs)
        .build()
        .unwrap();

    let nlmsg = NlmsghdrBuilder::default()
        .nl_type(Rtm::Delroute)
        .nl_flags(NlmF::REQUEST | NlmF::ACK)
        .nl_payload(NlPayload::Payload(rtmsg))
        .build()
        .map_err(|e| format!("Failed to build netlink message: {e}"))?;

    socket
        .send(&nlmsg)
        .map_err(|e| format!("Failed to send Netlink request: {e}"))?;
    recv_netlink_ack(&mut socket, "remove OS route")
}

/// Add an OS-level route via Netlink.
pub(crate) fn add_os_route(cidr: &str, dev: &str) -> Result<(), String> {
    let cidr = Cidr::from_str(cidr).map_err(|e| e.to_string())?;
    let if_index =
        get_if_index(dev).map_err(|e| format!("Failed to resolve interface {dev}: {e}"))?;

    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, Groups::empty())
        .map_err(|e| format!("Netlink connect failed: {e}"))?;

    let (rt_family, dst_bytes) = match cidr.addr() {
        std::net::IpAddr::V4(addr) => (RtAddrFamily::Inet, addr.octets().to_vec()),
        std::net::IpAddr::V6(addr) => (RtAddrFamily::Inet6, addr.octets().to_vec()),
    };

    let mut rtattrs = RtBuffer::new();
    rtattrs.push(
        RtattrBuilder::default()
            .rta_type(Rta::Dst)
            .rta_payload(dst_bytes)
            .build()
            .unwrap(),
    );
    #[allow(clippy::cast_possible_wrap)]
    rtattrs.push(
        RtattrBuilder::default()
            .rta_type(Rta::Oif)
            .rta_payload(if_index as i32)
            .build()
            .unwrap(),
    );

    let rtmsg = RtmsgBuilder::default()
        .rtm_family(rt_family)
        .rtm_dst_len(cidr.prefix_len())
        .rtm_src_len(0)
        .rtm_tos(0)
        .rtm_table(RtTable::Main)
        .rtm_protocol(Rtprot::Boot)
        .rtm_scope(RtScope::Universe)
        .rtm_type(Rtn::Unicast)
        .rtm_flags(RtmF::empty())
        .rtattrs(rtattrs)
        .build()
        .unwrap();

    let nlmsg = NlmsghdrBuilder::default()
        .nl_type(Rtm::Newroute)
        .nl_flags(NlmF::REQUEST | NlmF::CREATE | NlmF::EXCL | NlmF::ACK)
        .nl_payload(NlPayload::Payload(rtmsg))
        .build()
        .map_err(|e| format!("Failed to build netlink message: {e}"))?;

    socket
        .send(&nlmsg)
        .map_err(|e| format!("Failed to send Netlink request: {e}"))?;
    recv_netlink_ack(&mut socket, "add OS route")
}

/// Receive and check the Netlink ACK/error response.
///
/// `NLMSG_ERROR` (type 2) carries a 4-byte `i32` error code at the start of its
/// payload. Error 0 = success (pure ACK), negative = errno.
/// `-3` (ESRCH) after route delete and `-17` (EEXIST) after route add are
/// treated as success (idempotent operations).
fn recv_netlink_ack(socket: &mut NlSocketHandle, op: &str) -> Result<(), String> {
    let (mut iter, _groups) = socket
        .recv::<u16, neli::types::Buffer>()
        .map_err(|e| format!("Failed to receive Netlink ACK: {e}"))?;

    let Some(msg_result) = iter.next() else {
        return Err("Netlink socket closed unexpectedly".into());
    };
    let msg = msg_result.map_err(|e| format!("Netlink recv error: {e}"))?;

    // NLMSG_ERROR = 2
    if *msg.nl_type() == 2 {
        if let NlPayload::Payload(buf) = msg.nl_payload() {
            let bytes: &[u8] = buf.as_ref();
            if bytes.len() >= 4 {
                let error = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                // 0 = success, -3 = ESRCH (already gone), -17 = EEXIST (already present)
                if error == 0 || error == -3 || error == -17 {
                    Ok(())
                } else {
                    let err_msg = format!("Netlink error: {error}");
                    tracing::warn!("Failed to {op}: {err_msg}");
                    Err(err_msg)
                }
            } else {
                Err("Netlink ACK payload too short".into())
            }
        } else {
            Err("Unexpected payload in ACK".into())
        }
    } else {
        Err(format!("Unexpected message type: {}", msg.nl_type()))
    }
}

/// Delete a TUN interface by name via `ip link delete`.
///
/// Best-effort: errors are logged but not propagated. Used for cleanup
/// when a peer disconnects so stale TUN interfaces do not accumulate.
pub(crate) fn delete_tun(name: &str) {
    match std::process::Command::new("ip")
        .args(["link", "delete", name])
        .output()
    {
        Ok(output) if output.status.success() => {
            tracing::debug!("Deleted TUN interface {name}");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "Cannot find device" means it was already gone — not an error.
            if !stderr.contains("Cannot find device") {
                tracing::warn!("Failed to delete TUN {name}: {stderr}");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to run 'ip link delete {name}': {e}");
        }
    }
}

/// Enumerate non-loopback, globally-routable CIDRs on local interfaces.
///
/// Queries the kernel via `RTM_GETADDR` and returns network addresses in CIDR
/// notation with host bits masked (e.g. `10.99.2.4/24` → `10.99.2.0/24`).
/// Used to populate `Handshake.routes` so peers can install routes
/// automatically on connect.
///
/// Only `RT_SCOPE_UNIVERSE` (globally routable) addresses are included.
/// Loopback, link-local, unspecified, and multicast addresses are skipped.
pub(crate) fn enumerate_local_cidrs() -> Vec<String> {
    let socket = match NlSocketHandle::connect(NlFamily::Route, None, Groups::empty()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("netlink: cannot open socket for address enumeration: {e}");
            return Vec::new();
        }
    };

    let request = match NlmsghdrBuilder::default()
        .nl_type(Rtm::Getaddr)
        .nl_flags(NlmF::REQUEST | NlmF::DUMP)
        .nl_payload(NlPayload::Payload(
            IfaddrmsgBuilder::default()
                .ifa_family(RtAddrFamily::Unspecified)
                .ifa_prefixlen(0)
                .ifa_flags(IfaF::empty())
                .ifa_scope(RtScope::Universe)
                .ifa_index(0)
                .rtattrs(RtBuffer::new())
                .build()
                .unwrap(),
        ))
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("netlink: failed to build RTM_GETADDR request: {e}");
            return Vec::new();
        }
    };

    if let Err(e) = socket.send(&request) {
        tracing::warn!("netlink: failed to send RTM_GETADDR: {e}");
        return Vec::new();
    }

    let mut cidrs = Vec::new();

    let (iter, _groups) = match socket.recv::<Rtm, Ifaddrmsg>() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("netlink: failed to recv RTM_GETADDR: {e}");
            return Vec::new();
        }
    };

    for msg in iter {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("netlink: address enumeration iter error: {e}");
                break;
            }
        };

        let NlPayload::Payload(ifaddrmsg) = msg.nl_payload() else {
            continue;
        };

        // Only globally routable addresses.
        if *ifaddrmsg.ifa_scope() != RtScope::Universe {
            continue;
        }

        let prefix_len = *ifaddrmsg.ifa_prefixlen();
        if prefix_len == 0 {
            // Skip default routes.
            continue;
        }

        let handle = ifaddrmsg.rtattrs().get_attr_handle();

        // IFA_LOCAL is preferred for point-to-point links; IFA_ADDRESS is the
        // typical case for broadcast interfaces.
        let addr_bytes: &[u8] = match handle
            .get_attribute(Ifa::Local)
            .or_else(|| handle.get_attribute(Ifa::Address))
        {
            Some(attr) => attr.rta_payload().as_ref(),
            None => continue,
        };

        let addr: IpAddr = match addr_bytes {
            [a, b, c, d] => IpAddr::V4(std::net::Ipv4Addr::new(*a, *b, *c, *d)),
            bytes if bytes.len() == 16 => {
                let arr: [u8; 16] = match bytes.try_into() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                IpAddr::V6(std::net::Ipv6Addr::from(arr))
            }
            _ => continue,
        };

        // Belt-and-suspenders: the scope check above should already exclude
        // these, but be defensive.
        if addr.is_loopback() || is_link_local(addr) || addr.is_unspecified() || addr.is_multicast()
        {
            continue;
        }

        cidrs.push(format!(
            "{}/{prefix_len}",
            mask_to_network(addr, prefix_len)
        ));
    }

    cidrs.sort_unstable();
    cidrs.dedup();
    cidrs
}

/// Returns `true` if the address is link-local (169.254.0.0/16 or `fe80::/10`).
///
/// `IpAddr::is_link_local()` is not yet stable — this dispatches to the
/// per-family methods that are available.
fn is_link_local(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => a.is_link_local(),
        IpAddr::V6(a) => {
            // fe80::/10  (first 10 bits are 1111111010)
            let octets = a.octets();
            octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80
        }
    }
}

/// Mask an IP address down to its network address given a prefix length.
fn mask_to_network(addr: IpAddr, prefix_len: u8) -> IpAddr {
    match addr {
        IpAddr::V4(a) => {
            let bits = u32::from(a);
            let mask = if prefix_len >= 32 {
                u32::MAX
            } else {
                !((1u32 << (32 - prefix_len)) - 1)
            };
            IpAddr::V4(std::net::Ipv4Addr::from(bits & mask))
        }
        IpAddr::V6(a) => {
            let bits = u128::from(a);
            let mask = if prefix_len >= 128 {
                u128::MAX
            } else {
                !((1u128 << (128 - prefix_len)) - 1)
            };
            IpAddr::V6(std::net::Ipv6Addr::from(bits & mask))
        }
    }
}
