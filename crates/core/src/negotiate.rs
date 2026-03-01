//! Role negotiation for auto-configuration.
//!
//! Each node independently derives the same topology from the combined
//! handshake capabilities using deterministic, pure rules. No side-effects,
//! no I/O.

use wallhack_wire::data::Handshake;

use crate::NodeRole;

/// Result of role negotiation.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NegotiationResult {
    /// Role unambiguously determined.
    Resolved(NodeRole),
    /// Cannot determine role from current inputs alone.
    /// The node stays in `NodeRole::Indeterminate` and waits for a topology
    /// change or an operator hint (Phase 13d).
    Indeterminate { reason: &'static str },
}

impl std::fmt::Display for NegotiationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolved(role) => write!(f, "resolved({role:?})"),
            Self::Indeterminate { reason } => write!(f, "indeterminate({reason})"),
        }
    }
}

/// Derive the local node's role from the exchange of two `Handshake` messages.
///
/// **This function is pure**: same inputs → same output, always. No I/O, no
/// global state, no side effects.
///
/// Both sides call this function independently. Given the same `(local, peer)`
/// pair (with roles swapped on each side), both sides derive complementary
/// results — entry on one side implies exit on the other.
///
/// # Rules
///
/// Relay is unambiguous: any node started with both `--listen` and `--connect`
/// is always a relay, regardless of TUN capability or what the peer advertises.
///
/// When talking to a relay, the non-relay node resolves immediately from its
/// own TUN capability alone (entry if TUN-capable, exit otherwise). The relay
/// signals that the chain continues, so there is no need to wait.
///
/// For two non-relay nodes, role is determined purely by TUN capability:
/// - TUN-capable ↔ non-TUN → local = entry
/// - non-TUN ↔ TUN-capable → local = exit
/// - both TUN-capable or both non-TUN → Indeterminate
#[must_use]
pub fn negotiate(local: &Handshake, peer: &Handshake) -> NegotiationResult {
    let local_caps = local.capabilities.as_ref();
    let peer_caps = peer.capabilities.as_ref();

    let local_tun = local_caps.is_some_and(|c| c.tun_capable);
    let local_listen = local_caps.is_some_and(|c| c.listening);
    let local_connect = local_caps.is_some_and(|c| c.connecting);
    let local_relay = local_listen && local_connect;

    let peer_tun = peer_caps.is_some_and(|c| c.tun_capable);
    let peer_listen = peer_caps.is_some_and(|c| c.listening);
    let peer_connect = peer_caps.is_some_and(|c| c.connecting);
    let peer_relay = peer_listen && peer_connect;

    // Relay is unambiguous: if local is both listening and connecting, it is
    // always relay regardless of peer capabilities or TUN status.
    if local_relay {
        return NegotiationResult::Resolved(NodeRole::Relay);
    }

    // Peer of a relay: resolve from local TUN capability alone.
    // The relay's presence signals the chain continues through it.
    if peer_relay {
        return if local_tun {
            NegotiationResult::Resolved(NodeRole::Entry)
        } else {
            NegotiationResult::Resolved(NodeRole::Exit)
        };
    }

    // Two non-relay nodes: entry/exit determined by TUN capability.
    match (local_tun, peer_tun) {
        (true, false) => NegotiationResult::Resolved(NodeRole::Entry),
        (false, true) => NegotiationResult::Resolved(NodeRole::Exit),
        (true, true) => NegotiationResult::Indeterminate {
            reason: "both peers are TUN-capable; use --prefer or --fixed-role to resolve",
        },
        (false, false) => NegotiationResult::Indeterminate {
            reason: "neither peer is TUN-capable; no node can create a TUN interface",
        },
    }
}

#[cfg(test)]
mod tests {
    use wallhack_wire::data::{Capabilities, Handshake};

    use super::*;
    use crate::NodeRole;

    fn hs(tun_capable: bool, listening: bool, connecting: bool) -> Handshake {
        Handshake {
            capabilities: Some(Capabilities {
                tun_capable,
                listening,
                connecting,
            }),
            name: String::new(),
            version: String::new(),
            psk_proof: vec![],
            routes: vec![],
            hint: None,
        }
    }

    fn resolved(role: NodeRole) -> NegotiationResult {
        NegotiationResult::Resolved(role)
    }

    fn is_indeterminate(r: &NegotiationResult) -> bool {
        matches!(r, NegotiationResult::Indeterminate { .. })
    }

    // -------------------------------------------------------------------------
    // Topology table from 13c task doc
    // (L = listening, C = connecting, T = tun_capable)
    // -------------------------------------------------------------------------

    /// TUN-capable listener ↔ non-TUN connector → entry / exit
    #[test]
    fn tun_listen_vs_nontun_connect() {
        let local = hs(true, true, false);
        let peer = hs(false, false, true);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Entry));
        // symmetry: peer sees exit
        assert_eq!(negotiate(&peer, &local), resolved(NodeRole::Exit));
    }

    /// non-TUN connector ↔ TUN-capable listener → exit / entry
    #[test]
    fn nontun_connect_vs_tun_listen() {
        let local = hs(false, false, true);
        let peer = hs(true, true, false);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Exit));
        assert_eq!(negotiate(&peer, &local), resolved(NodeRole::Entry));
    }

    /// TUN-capable connector ↔ non-TUN listener → entry / exit
    #[test]
    fn tun_connect_vs_nontun_listen() {
        let local = hs(true, false, true);
        let peer = hs(false, true, false);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Entry));
        assert_eq!(negotiate(&peer, &local), resolved(NodeRole::Exit));
    }

    /// non-TUN listener ↔ TUN-capable connector → exit / entry
    #[test]
    fn nontun_listen_vs_tun_connect() {
        let local = hs(false, true, false);
        let peer = hs(true, false, true);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Exit));
        assert_eq!(negotiate(&peer, &local), resolved(NodeRole::Entry));
    }

    /// TUN-capable listener ↔ TUN-capable connector → indeterminate (symmetric)
    #[test]
    fn both_tun_capable_indeterminate() {
        let local = hs(true, true, false);
        let peer = hs(true, false, true);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// TUN-capable connector ↔ TUN-capable listener → indeterminate
    #[test]
    fn both_tun_connect_listen_indeterminate() {
        let local = hs(true, false, true);
        let peer = hs(true, true, false);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// non-TUN listener ↔ non-TUN connector → indeterminate (no entry possible)
    #[test]
    fn neither_tun_listen_vs_connect_indeterminate() {
        let local = hs(false, true, false);
        let peer = hs(false, false, true);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// non-TUN connector ↔ non-TUN listener → indeterminate
    #[test]
    fn neither_tun_connect_vs_listen_indeterminate() {
        let local = hs(false, false, true);
        let peer = hs(false, true, false);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// Relay (both) ↔ any → local = relay (regardless of peer)
    #[test]
    fn relay_vs_nontun_connector() {
        let local = hs(false, true, true); // relay
        let peer = hs(false, false, true);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Relay));
    }

    #[test]
    fn relay_vs_tun_listener() {
        let local = hs(false, true, true); // relay
        let peer = hs(true, true, false);
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Relay));
    }

    #[test]
    fn relay_vs_relay() {
        let local = hs(false, true, true); // relay
        let peer = hs(false, true, true); // also relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Relay));
        assert_eq!(negotiate(&peer, &local), resolved(NodeRole::Relay));
    }

    /// TUN-capable node ↔ relay → entry
    #[test]
    fn tun_listen_vs_relay() {
        let local = hs(true, true, false);
        let peer = hs(false, true, true); // relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Entry));
    }

    #[test]
    fn tun_connect_vs_relay() {
        let local = hs(true, false, true);
        let peer = hs(false, true, true); // relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Entry));
    }

    /// non-TUN node ↔ relay → exit
    #[test]
    fn nontun_listen_vs_relay() {
        let local = hs(false, true, false);
        let peer = hs(false, true, true); // relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Exit));
    }

    #[test]
    fn nontun_connect_vs_relay() {
        let local = hs(false, false, true);
        let peer = hs(false, true, true); // relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Exit));
    }

    // -------------------------------------------------------------------------
    // Properties
    // -------------------------------------------------------------------------

    /// Determinism: same inputs always produce same output.
    #[test]
    fn determinism() {
        let local = hs(true, true, false);
        let peer = hs(false, false, true);
        let r1 = negotiate(&local, &peer);
        let r2 = negotiate(&local, &peer);
        assert_eq!(r1, r2);
    }

    /// Symmetry: if local = entry, swapped = exit; if indeterminate, both indeterminate.
    #[test]
    fn symmetry_entry_exit() {
        let a = hs(true, true, false);
        let b = hs(false, false, true);
        assert_eq!(negotiate(&a, &b), resolved(NodeRole::Entry));
        assert_eq!(negotiate(&b, &a), resolved(NodeRole::Exit));
    }

    #[test]
    fn symmetry_indeterminate() {
        let a = hs(true, true, false);
        let b = hs(true, false, true);
        assert!(is_indeterminate(&negotiate(&a, &b)));
        assert!(is_indeterminate(&negotiate(&b, &a)));
    }

    /// Missing capabilities (None) → treated as all-false.
    #[test]
    fn missing_capabilities_treated_as_false() {
        let local = Handshake {
            capabilities: None,
            name: String::new(),
            version: String::new(),
            psk_proof: vec![],
            routes: vec![],
            hint: None,
        };
        let peer = hs(true, true, false);
        // local has no capabilities → non-TUN, not relay
        assert_eq!(negotiate(&local, &peer), resolved(NodeRole::Exit));
    }
}
