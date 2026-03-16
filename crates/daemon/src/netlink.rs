//! Netlink helpers: OS route management and local address enumeration.

use std::{net::IpAddr, str::FromStr};

use neli::{
    consts::{
        nl::{NlmF, NlmFFlags},
        rtnl::{Ifa, IfaFFlags, RtAddrFamily, RtScope, RtTable, Rta, Rtm, RtmFFlags, Rtn, Rtprot},
        socket::NlFamily,
    },
    err::Nlmsgerr,
    nl::{NlPayload, Nlmsghdr},
    rtnl::{Ifaddrmsg, Rtattr, Rtmsg},
    socket::NlSocketHandle,
    types::RtBuffer,
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

    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[])
        .map_err(|e| format!("Netlink connect failed: {e}"))?;

    let (rt_family, dst_bytes) = match cidr.addr() {
        std::net::IpAddr::V4(addr) => (RtAddrFamily::Inet, addr.octets().to_vec()),
        std::net::IpAddr::V6(addr) => (RtAddrFamily::Inet6, addr.octets().to_vec()),
    };

    let mut rtattrs = RtBuffer::new();
    rtattrs.push(Rtattr::new(None, Rta::Dst, dst_bytes).unwrap());
    #[allow(clippy::cast_possible_wrap)]
    rtattrs.push(Rtattr::new(None, Rta::Oif, if_index as i32).unwrap());

    let rtmsg = Rtmsg {
        rtm_family: rt_family,
        rtm_dst_len: cidr.prefix_len(),
        rtm_src_len: 0,
        rtm_tos: 0,
        rtm_table: RtTable::Main,
        rtm_protocol: Rtprot::Boot,
        rtm_scope: RtScope::Universe,
        rtm_type: Rtn::Unicast,
        rtm_flags: RtmFFlags::empty(),
        rtattrs,
    };

    let nlmsg = Nlmsghdr::new(
        None,
        Rtm::Delroute,
        NlmFFlags::new(&[NlmF::Request, NlmF::Ack]),
        None,
        None,
        NlPayload::Payload(rtmsg),
    );

    match socket.send(nlmsg) {
        Ok(()) => match socket.recv::<u16, Nlmsgerr<Rtm, Rtmsg>>() {
            Ok(Some(msg)) => {
                if msg.nl_type == 2 {
                    if let NlPayload::Payload(e) = msg.nl_payload {
                        if e.error == 0 || e.error == -3 {
                            // Success or ESRCH (not found — already gone)
                            Ok(())
                        } else {
                            let err_msg = format!("Netlink error: {}", e.error);
                            tracing::warn!("Failed to remove OS route: {}", err_msg);
                            Err(err_msg)
                        }
                    } else {
                        Err("Unexpected payload in ACK".into())
                    }
                } else {
                    Err(format!("Unexpected message type: {}", msg.nl_type))
                }
            }
            Ok(None) => Err("Netlink socket closed unexpectedly".into()),
            Err(e) => Err(format!("Failed to receive Netlink ACK: {e}")),
        },
        Err(e) => Err(format!("Failed to send Netlink request: {e}")),
    }
}

/// Add an OS-level route via Netlink.
pub(crate) fn add_os_route(cidr: &str, dev: &str) -> Result<(), String> {
    let cidr = Cidr::from_str(cidr).map_err(|e| e.to_string())?;
    let if_index =
        get_if_index(dev).map_err(|e| format!("Failed to resolve interface {dev}: {e}"))?;

    let mut socket = NlSocketHandle::connect(NlFamily::Route, None, &[])
        .map_err(|e| format!("Netlink connect failed: {e}"))?;

    let (rt_family, dst_bytes) = match cidr.addr() {
        std::net::IpAddr::V4(addr) => (RtAddrFamily::Inet, addr.octets().to_vec()),
        std::net::IpAddr::V6(addr) => (RtAddrFamily::Inet6, addr.octets().to_vec()),
    };

    let mut rtattrs = RtBuffer::new();
    rtattrs.push(Rtattr::new(None, Rta::Dst, dst_bytes).unwrap());
    #[allow(clippy::cast_possible_wrap)]
    rtattrs.push(Rtattr::new(None, Rta::Oif, if_index as i32).unwrap());

    let rtmsg = Rtmsg {
        rtm_family: rt_family,
        rtm_dst_len: cidr.prefix_len(),
        rtm_src_len: 0,
        rtm_tos: 0,
        rtm_table: RtTable::Main,
        rtm_protocol: Rtprot::Boot,
        rtm_scope: RtScope::Universe,
        rtm_type: Rtn::Unicast,
        rtm_flags: RtmFFlags::empty(),
        rtattrs,
    };

    let nlmsg = Nlmsghdr::new(
        None,
        Rtm::Newroute,
        NlmFFlags::new(&[NlmF::Request, NlmF::Create, NlmF::Excl, NlmF::Ack]),
        None,
        None,
        NlPayload::Payload(rtmsg),
    );

    match socket.send(nlmsg) {
        Ok(()) => match socket.recv::<u16, Nlmsgerr<Rtm, Rtmsg>>() {
            Ok(Some(msg)) => {
                if msg.nl_type == 2 {
                    if let NlPayload::Payload(e) = msg.nl_payload {
                        if e.error == 0 || e.error == -17 {
                            // Success or EEXIST (route already present)
                            Ok(())
                        } else {
                            let err_msg = format!("Netlink error: {}", e.error);
                            tracing::warn!("Failed to add OS route: {}", err_msg);
                            Err(err_msg)
                        }
                    } else {
                        Err("Unexpected payload in ACK".into())
                    }
                } else {
                    Err(format!("Unexpected message type: {}", msg.nl_type))
                }
            }
            Ok(None) => Err("Netlink socket closed unexpectedly".into()),
            Err(e) => Err(format!("Failed to receive Netlink ACK: {e}")),
        },
        Err(e) => Err(format!("Failed to send Netlink request: {e}")),
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
    let mut socket = match NlSocketHandle::connect(NlFamily::Route, None, &[]) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("netlink: cannot open socket for address enumeration: {e}");
            return Vec::new();
        }
    };

    let request = Nlmsghdr::new(
        None,
        Rtm::Getaddr,
        NlmFFlags::new(&[NlmF::Request, NlmF::Dump]),
        None,
        None,
        NlPayload::Payload(Ifaddrmsg {
            ifa_family: RtAddrFamily::Unspecified,
            ifa_prefixlen: 0,
            ifa_flags: IfaFFlags::empty(),
            ifa_scope: 0,
            ifa_index: 0,
            rtattrs: RtBuffer::new(),
        }),
    );

    if let Err(e) = socket.send(request) {
        tracing::warn!("netlink: failed to send RTM_GETADDR: {e}");
        return Vec::new();
    }

    let mut cidrs = Vec::new();

    for msg in socket.iter::<Rtm, Ifaddrmsg>(false) {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("netlink: address enumeration iter error: {e}");
                break;
            }
        };

        let NlPayload::Payload(ifaddrmsg) = msg.nl_payload else {
            continue;
        };

        // Only globally routable addresses (RT_SCOPE_UNIVERSE = 0).
        if ifaddrmsg.ifa_scope != 0 {
            continue;
        }

        let prefix_len = ifaddrmsg.ifa_prefixlen;
        if prefix_len == 0 {
            // Skip default routes.
            continue;
        }

        let handle = ifaddrmsg.rtattrs.get_attr_handle();

        // IFA_LOCAL is preferred for point-to-point links; IFA_ADDRESS is the
        // typical case for broadcast interfaces.
        let addr_bytes: &[u8] = match handle
            .get_attribute(Ifa::Local)
            .or_else(|| handle.get_attribute(Ifa::Address))
        {
            Some(attr) => attr.rta_payload.as_ref(),
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
