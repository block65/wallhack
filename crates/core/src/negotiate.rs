//! Role negotiation for auto-configuration.
//!
//! Each node independently derives the same topology from the combined
//! handshake capabilities using deterministic, pure rules. No side-effects,
//! no I/O.

use wallhack_wire::data::{Handshake, HintLevel, NodeRole as ProtoNodeRole, RoleHint};

use crate::NodeRole;

/// Result of role negotiation.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NegotiationResult {
    /// Role unambiguously determined.
    Resolved {
        /// The negotiated role.
        role: NodeRole,
        /// Human-readable explanation of why this role was selected.
        reason: &'static str,
    },
    /// Cannot determine role from current inputs alone.
    /// The node stays in `NodeRole::Indeterminate` and waits for a topology
    /// change or an operator hint (Phase 13d).
    Indeterminate { reason: &'static str },
}

impl std::fmt::Display for NegotiationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolved { role, reason } => write!(f, "resolved({role:?}, {reason})"),
            Self::Indeterminate { reason } => write!(f, "indeterminate({reason})"),
        }
    }
}

/// Extract hint level and target from a `RoleHint`, returning `None` for
/// unspecified or invalid values.
fn parse_hint(hint: Option<&RoleHint>) -> Option<(HintLevel, NodeRole)> {
    let h = hint?;
    let level = HintLevel::try_from(h.level).ok()?;
    if level == HintLevel::Unspecified {
        return None;
    }
    let target = ProtoNodeRole::try_from(h.target).ok()?;
    if target == ProtoNodeRole::RoleIndeterminate {
        return None;
    }
    Some((level, target.into()))
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
/// # Rules (in priority order)
///
/// 1. **FIXED hint** — checked first. If local has a FIXED hint, return
///    the target role immediately. If both sides are FIXED to the same role,
///    return Indeterminate (conflict detected at runtime, not startup).
///
/// 2. **Capability-based rules** — relay, TUN asymmetry. If these produce an
///    unambiguous result, hints don't override it.
///
/// 3. **EXCLUDE hint** — removes a role from the local candidate set, then
///    re-evaluates.
///
/// 4. **PREFER hint** — breaks ambiguity when capability rules alone
///    produce Indeterminate.
#[must_use]
pub fn negotiate(local: &Handshake, peer: &Handshake) -> NegotiationResult {
    let local_hint = parse_hint(local.hint.as_ref());
    let peer_hint = parse_hint(peer.hint.as_ref());

    // 1. FIXED — override everything.
    if let Some((HintLevel::Fixed, target)) = local_hint {
        // If the peer is also FIXED to the same role, that's a conflict.
        if let Some((HintLevel::Fixed, peer_target)) = peer_hint
            && target == peer_target
        {
            return NegotiationResult::Indeterminate {
                reason: "both peers have fixed hints for the same role",
            };
        }
        return NegotiationResult::Resolved {
            role: target,
            reason: "local has a fixed role hint",
        };
    }

    // 2. Capability-based rules.
    let cap_result = negotiate_from_capabilities(local, peer);

    // If capabilities gave a clear answer, return it.
    if let NegotiationResult::Resolved { .. } = &cap_result {
        return cap_result;
    }

    // 3. EXCLUDE — remove a role from consideration.
    if let Some((HintLevel::Exclude, excluded)) = local_hint {
        return negotiate_with_exclude(local, peer, excluded);
    }

    // 4. PREFER — break ambiguity.
    let local_prefer = match local_hint {
        Some((HintLevel::Prefer, target)) => Some(target),
        _ => None,
    };
    let peer_prefer = match peer_hint {
        Some((HintLevel::Prefer, target)) => Some(target),
        _ => None,
    };

    if local_prefer.is_some() || peer_prefer.is_some() {
        return negotiate_with_prefer(local, peer, local_prefer, peer_prefer);
    }

    cap_result
}

/// Pure capability-based negotiation (no hints).
fn negotiate_from_capabilities(local: &Handshake, peer: &Handshake) -> NegotiationResult {
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
        return NegotiationResult::Resolved {
            role: NodeRole::Relay,
            reason: "local is relay (both listening and connecting)",
        };
    }

    // Peer of a relay: resolve from local TUN capability alone.
    // The relay's presence signals the chain continues through it.
    if peer_relay {
        return if local_tun {
            NegotiationResult::Resolved {
                role: NodeRole::Entry,
                reason: "peer is relay; local has TUN capability",
            }
        } else {
            NegotiationResult::Resolved {
                role: NodeRole::Exit,
                reason: "peer is relay; local lacks TUN",
            }
        };
    }

    // Two non-relay nodes: entry/exit determined by TUN capability.
    match (local_tun, peer_tun) {
        (true, false) => NegotiationResult::Resolved {
            role: NodeRole::Entry,
            reason: "TUN capability asymmetry: local has TUN, peer does not",
        },
        (false, true) => NegotiationResult::Resolved {
            role: NodeRole::Exit,
            reason: "TUN capability asymmetry: peer has TUN, local does not",
        },
        (true, true) => {
            let local_interactive = local_caps.is_some_and(|c| c.interactive);
            let peer_interactive = peer_caps.is_some_and(|c| c.interactive);
            match (local_interactive, peer_interactive) {
                (true, false) => NegotiationResult::Resolved {
                    role: NodeRole::Entry,
                    reason: "interactive terminal (human-in-the-loop)",
                },
                (false, true) => NegotiationResult::Resolved {
                    role: NodeRole::Exit,
                    reason: "peer has interactive terminal",
                },
                _ => NegotiationResult::Indeterminate {
                    reason: "both peers are TUN-capable; set a preferred or fixed role to resolve",
                },
            }
        }
        (false, false) => NegotiationResult::Indeterminate {
            reason: "neither peer has TUN capability",
        },
    }
}

/// Complement of a role in entry/exit pair.
fn complement(role: NodeRole) -> Option<NodeRole> {
    match role {
        NodeRole::Entry => Some(NodeRole::Exit),
        NodeRole::Exit => Some(NodeRole::Entry),
        _ => None,
    }
}

/// Negotiate with a local EXCLUDE hint.
///
/// Only the local node's EXCLUDE is applied here. The peer's EXCLUDE (if any)
/// is applied independently when the peer calls `negotiate()` with swapped
/// arguments. This means a local EXCLUDE can resolve this side while the peer
/// may still be Indeterminate — that's by design: each side acts on its own
/// hint, and convergence happens when both sides have enough information.
fn negotiate_with_exclude(
    local: &Handshake,
    peer: &Handshake,
    excluded: NodeRole,
) -> NegotiationResult {
    let local_tun = local.capabilities.as_ref().is_some_and(|c| c.tun_capable);

    // If the excluded role has a complement and the node could plausibly
    // fill it, resolve to the complement.
    if let Some(remaining) = complement(excluded) {
        // Sanity: for entry we need TUN capability.
        if remaining == NodeRole::Entry && !local_tun {
            return NegotiationResult::Indeterminate {
                reason: "excluded role leaves entry, but node lacks TUN capability",
            };
        }
        // Check the peer can plausibly take the excluded role. For "exclude
        // exit" the peer needs to be able to be exit (non-relay peer is fine).
        let peer_tun = peer.capabilities.as_ref().is_some_and(|c| c.tun_capable);
        if excluded == NodeRole::Entry && !peer_tun {
            return NegotiationResult::Indeterminate {
                reason: "excluded entry for local, but peer also lacks TUN capability",
            };
        }
        return NegotiationResult::Resolved {
            role: remaining,
            reason: "exclude hint narrows to complement role",
        };
    }

    // Excluding relay when local isn't a relay is a no-op for two-node
    // entry/exit negotiation — fall through to capability-based.
    negotiate_from_capabilities(local, peer)
}

/// Negotiate with PREFER hints.
fn negotiate_with_prefer(
    local: &Handshake,
    peer: &Handshake,
    local_prefer: Option<NodeRole>,
    peer_prefer: Option<NodeRole>,
) -> NegotiationResult {
    let local_tun = local.capabilities.as_ref().is_some_and(|c| c.tun_capable);

    // Peer prefers relay → signals the chain continues through them, so a
    // TUN-capable local resolves to entry.
    if peer_prefer == Some(NodeRole::Relay) && local_tun {
        return NegotiationResult::Resolved {
            role: NodeRole::Entry,
            reason: "peer prefers relay; local has TUN capability",
        };
    }

    match (local_prefer, peer_prefer) {
        // Both prefer the same contested role → still ambiguous.
        (Some(l), Some(p)) if l == p => NegotiationResult::Indeterminate {
            reason: "both peers prefer the same role",
        },
        // They prefer different roles → each gets what they want.
        (Some(role), Some(_)) => NegotiationResult::Resolved {
            role,
            reason: "local prefer hint resolved (peers prefer different roles)",
        },
        // Only local prefers → local gets it.
        (Some(role), None) => NegotiationResult::Resolved {
            role,
            reason: "local prefer hint resolved (uncontested)",
        },
        // Only peer prefers → local gets the complement.
        (None, Some(peer_target)) => {
            if let Some(role) = complement(peer_target) {
                NegotiationResult::Resolved {
                    role,
                    reason: "peer prefer hint; local takes complement role",
                }
            } else {
                negotiate_from_capabilities(local, peer)
            }
        }
        (None, None) => negotiate_from_capabilities(local, peer),
    }
}

#[cfg(test)]
mod tests {
    use wallhack_wire::data::{
        Capabilities, Handshake, HintLevel, NodeRole as ProtoNodeRole, RoleHint,
    };

    use super::*;
    use crate::NodeRole;

    fn hs(tun_capable: bool, listening: bool, connecting: bool) -> Handshake {
        hs_hint(tun_capable, listening, connecting, None)
    }

    fn hs_hint(
        tun_capable: bool,
        listening: bool,
        connecting: bool,
        hint: Option<RoleHint>,
    ) -> Handshake {
        Handshake {
            capabilities: Some(Capabilities {
                tun_capable,
                listening,
                connecting,
                interactive: false,
            }),
            name: String::new(),
            version: String::new(),
            psk_proof: vec![],
            routes: vec![],
            hint,
        }
    }

    fn role_hint(level: HintLevel, target: ProtoNodeRole) -> RoleHint {
        RoleHint {
            level: level.into(),
            target: target.into(),
        }
    }

    fn assert_resolved(result: &NegotiationResult, expected_role: NodeRole) {
        match result {
            NegotiationResult::Resolved { role, .. } => assert_eq!(
                *role, expected_role,
                "expected {expected_role:?}, got {role:?}"
            ),
            NegotiationResult::Indeterminate { reason } => {
                panic!("expected Resolved({expected_role:?}), got indeterminate({reason})");
            }
        }
    }

    fn is_indeterminate(r: &NegotiationResult) -> bool {
        matches!(r, NegotiationResult::Indeterminate { .. })
    }

    // REASON: This is a test helper that mirrors the Capabilities wire format; the
    // bools are distinct flags, not a state machine. Only used in tests.
    #[allow(clippy::fn_params_excessive_bools)]
    fn hs_interactive(
        tun_capable: bool,
        listening: bool,
        connecting: bool,
        interactive: bool,
    ) -> Handshake {
        Handshake {
            capabilities: Some(Capabilities {
                tun_capable,
                listening,
                connecting,
                interactive,
            }),
            name: String::new(),
            version: String::new(),
            psk_proof: vec![],
            routes: vec![],
            hint: None,
        }
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
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        // symmetry: peer sees exit
        assert_resolved(&negotiate(&peer, &local), NodeRole::Exit);
    }

    /// non-TUN connector ↔ TUN-capable listener → exit / entry
    #[test]
    fn nontun_connect_vs_tun_listen() {
        let local = hs(false, false, true);
        let peer = hs(true, true, false);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Entry);
    }

    /// TUN-capable connector ↔ non-TUN listener → entry / exit
    #[test]
    fn tun_connect_vs_nontun_listen() {
        let local = hs(true, false, true);
        let peer = hs(false, true, false);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Exit);
    }

    /// non-TUN listener ↔ TUN-capable connector → exit / entry
    #[test]
    fn nontun_listen_vs_tun_connect() {
        let local = hs(false, true, false);
        let peer = hs(true, false, true);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Entry);
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
        assert_resolved(&negotiate(&local, &peer), NodeRole::Relay);
    }

    #[test]
    fn relay_vs_tun_listener() {
        let local = hs(false, true, true); // relay
        let peer = hs(true, true, false);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Relay);
    }

    #[test]
    fn relay_vs_relay() {
        let local = hs(false, true, true); // relay
        let peer = hs(false, true, true); // also relay
        assert_resolved(&negotiate(&local, &peer), NodeRole::Relay);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Relay);
    }

    /// TUN-capable node ↔ relay → entry
    #[test]
    fn tun_listen_vs_relay() {
        let local = hs(true, true, false);
        let peer = hs(false, true, true); // relay
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
    }

    #[test]
    fn tun_connect_vs_relay() {
        let local = hs(true, false, true);
        let peer = hs(false, true, true); // relay
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
    }

    /// non-TUN node ↔ relay → exit
    #[test]
    fn nontun_listen_vs_relay() {
        let local = hs(false, true, false);
        let peer = hs(false, true, true); // relay
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
    }

    #[test]
    fn nontun_connect_vs_relay() {
        let local = hs(false, false, true);
        let peer = hs(false, true, true); // relay
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
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
        assert_resolved(&negotiate(&a, &b), NodeRole::Entry);
        assert_resolved(&negotiate(&b, &a), NodeRole::Exit);
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
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
    }

    // -------------------------------------------------------------------------
    // Phase 13d: Hint tests
    // -------------------------------------------------------------------------

    /// PREFER breaks ambiguity: one side prefers entry → resolved.
    #[test]
    fn prefer_breaks_ambiguity() {
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleEntry)),
        );
        let peer = hs(true, false, true);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        // Symmetry: peer sees exit.
        assert_resolved(&negotiate(&peer, &local), NodeRole::Exit);
    }

    /// PREFER ignored when topology is unambiguous.
    #[test]
    fn prefer_ignored_when_unambiguous() {
        // local is TUN, peer is not → local is entry regardless of prefer exit hint.
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleExit)),
        );
        let peer = hs(false, false, true);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
    }

    /// Conflicting PREFER: both prefer entry → Indeterminate.
    #[test]
    fn conflicting_prefer_indeterminate() {
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleEntry)),
        );
        let peer = hs_hint(
            true,
            false,
            true,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleEntry)),
        );
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// Different PREFER: one prefers entry, other prefers exit → both resolve.
    #[test]
    fn different_prefer_both_resolve() {
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleEntry)),
        );
        let peer = hs_hint(
            true,
            false,
            true,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleExit)),
        );
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Exit);
    }

    /// EXCLUDE removes a role from consideration.
    #[test]
    fn exclude_removes_role() {
        // Both TUN-capable → normally indeterminate. Local excludes entry → local = exit.
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Exclude, ProtoNodeRole::RoleEntry)),
        );
        let peer = hs(true, false, true);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
    }

    /// EXCLUDE leaves no valid role → Indeterminate.
    #[test]
    fn exclude_no_valid_role_indeterminate() {
        // Local is not TUN-capable and excludes exit → can't be entry (no TUN), can't be exit (excluded).
        let local = hs_hint(
            false,
            true,
            false,
            Some(role_hint(HintLevel::Exclude, ProtoNodeRole::RoleExit)),
        );
        let peer = hs(false, false, true);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
    }

    /// FIXED overrides capability-based result.
    #[test]
    fn fixed_overrides_capabilities() {
        // TUN-capable would normally be entry, but FIXED exit overrides.
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Fixed, ProtoNodeRole::RoleExit)),
        );
        let peer = hs(false, false, true);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
    }

    /// FIXED + both fixed to same role → Indeterminate.
    #[test]
    fn fixed_same_role_indeterminate() {
        let local = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Fixed, ProtoNodeRole::RoleEntry)),
        );
        let peer = hs_hint(
            true,
            false,
            true,
            Some(role_hint(HintLevel::Fixed, ProtoNodeRole::RoleEntry)),
        );
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// PREFER relay early signal: peer prefers relay → local resolves to entry.
    #[test]
    fn prefer_relay_early_signal() {
        let local = hs(true, true, false);
        let peer = hs_hint(
            true,
            false,
            true,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleRelay)),
        );
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        // Symmetry: peer prefers relay, so peer resolves to relay.
        assert_resolved(&negotiate(&peer, &local), NodeRole::Relay);
    }

    /// Symmetry verified for all hint scenarios.
    #[test]
    fn hint_symmetry_prefer() {
        let a = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Prefer, ProtoNodeRole::RoleEntry)),
        );
        let b = hs(true, false, true);
        assert_resolved(&negotiate(&a, &b), NodeRole::Entry);
        assert_resolved(&negotiate(&b, &a), NodeRole::Exit);
    }

    #[test]
    fn hint_symmetry_fixed() {
        let a = hs_hint(
            true,
            true,
            false,
            Some(role_hint(HintLevel::Fixed, ProtoNodeRole::RoleEntry)),
        );
        let b = hs_hint(
            false,
            false,
            true,
            Some(role_hint(HintLevel::Fixed, ProtoNodeRole::RoleExit)),
        );
        assert_resolved(&negotiate(&a, &b), NodeRole::Entry);
        assert_resolved(&negotiate(&b, &a), NodeRole::Exit);
    }

    // -------------------------------------------------------------------------
    // Interactive tiebreaker tests
    // -------------------------------------------------------------------------

    /// One peer is interactive, the other is not; both are TUN-capable → resolves.
    #[test]
    fn interactive_breaks_tun_ambiguity() {
        let local = hs_interactive(true, true, false, true); // interactive
        let peer = hs_interactive(true, false, true, false); // not interactive
        assert_resolved(&negotiate(&local, &peer), NodeRole::Entry);
        // Symmetry: peer sees exit.
        assert_resolved(&negotiate(&peer, &local), NodeRole::Exit);
    }

    /// Both peers are interactive and TUN-capable → still indeterminate.
    #[test]
    fn both_interactive_still_indeterminate() {
        let local = hs_interactive(true, true, false, true);
        let peer = hs_interactive(true, false, true, true);
        assert!(is_indeterminate(&negotiate(&local, &peer)));
        assert!(is_indeterminate(&negotiate(&peer, &local)));
    }

    /// Interactive flag has no effect when peers are not both TUN-capable.
    #[test]
    fn interactive_irrelevant_without_tun() {
        // Local is interactive but not TUN-capable; peer is TUN-capable.
        // TUN asymmetry should determine the role, not interactive.
        let local = hs_interactive(false, true, false, true);
        let peer = hs_interactive(true, false, true, false);
        assert_resolved(&negotiate(&local, &peer), NodeRole::Exit);
        assert_resolved(&negotiate(&peer, &local), NodeRole::Entry);
    }
}
