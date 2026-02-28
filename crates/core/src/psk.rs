//! PSK proof construction and verification via TLS channel binding.
//!
//! Builds on the generic HMAC functions in [`crate::hmac`] to prove PSK
//! knowledge without transmitting the key. The proof is bound to the TLS
//! session via `export_keying_material()` (RFC 9266, `tls-exporter`).

use crate::hmac;

/// TLS exporter label for channel binding (RFC 9266, `tls-exporter`).
pub const CHANNEL_BINDING_LABEL: &[u8] = b"EXPORTER-Channel-Binding";

/// Length of the channel binding output in bytes (SHA-256 output size).
pub const CHANNEL_BINDING_LEN: usize = 32;

/// Extract TLS channel binding from a QUIC connection.
///
/// Returns `None` if the export fails (e.g. connection not yet established).
#[cfg(feature = "quic")]
#[must_use]
pub fn channel_binding_quic(conn: &quinn::Connection) -> Option<[u8; CHANNEL_BINDING_LEN]> {
    let mut output = [0u8; CHANNEL_BINDING_LEN];
    conn.export_keying_material(&mut output, CHANNEL_BINDING_LABEL, b"")
        .ok()?;
    Some(output)
}

/// Extract TLS channel binding from a rustls `ClientConnection`.
#[must_use]
pub fn channel_binding_rustls_client(
    conn: &rustls::ClientConnection,
) -> Option<[u8; CHANNEL_BINDING_LEN]> {
    let mut output = [0u8; CHANNEL_BINDING_LEN];
    conn.export_keying_material(&mut output, CHANNEL_BINDING_LABEL, Some(b""))
        .ok()?;
    Some(output)
}

/// Extract TLS channel binding from a rustls `ServerConnection`.
#[must_use]
pub fn channel_binding_rustls_server(
    conn: &rustls::ServerConnection,
) -> Option<[u8; CHANNEL_BINDING_LEN]> {
    let mut output = [0u8; CHANNEL_BINDING_LEN];
    conn.export_keying_material(&mut output, CHANNEL_BINDING_LABEL, Some(b""))
        .ok()?;
    Some(output)
}

/// Compute a PSK proof over a handshake and channel binding.
///
/// Returns the HMAC-SHA256 proof bytes. The caller sets this as
/// `Handshake.psk_proof` before sending.
#[must_use]
pub fn compute_proof(
    psk: &[u8],
    channel_binding: &[u8; CHANNEL_BINDING_LEN],
    handshake: &wallhack_wire::data::Handshake,
) -> Vec<u8> {
    let message = handshake.serialize_for_proof();
    hmac::compute(psk, channel_binding, &message)
}

/// Verify a peer's PSK proof against the expected PSK and channel binding.
///
/// Returns `true` if the proof is valid.
#[must_use]
pub fn verify_proof(
    psk: &[u8],
    channel_binding: &[u8; CHANNEL_BINDING_LEN],
    handshake: &wallhack_wire::data::Handshake,
) -> bool {
    let message = handshake.serialize_for_proof();
    hmac::verify(psk, channel_binding, &message, &handshake.psk_proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallhack_wire::data::Handshake;

    fn test_handshake() -> Handshake {
        Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: true,
                listening: true,
                connecting: false,
            }),
            name: "test-node".to_string(),
            version: "0.1.0".to_string(),
            psk_proof: Vec::new(),
            routes: vec!["10.0.0.0/8".to_string()],
            hint: None,
        }
    }

    #[test]
    fn handshake_proof_round_trip() {
        let psk = b"my-psk";
        let binding: [u8; CHANNEL_BINDING_LEN] = *b"tls-channel-binding-material-32!";
        let mut handshake = test_handshake();

        handshake.psk_proof = compute_proof(psk, &binding, &handshake);
        assert!(verify_proof(psk, &binding, &handshake));
    }

    #[test]
    fn wrong_psk_rejected() {
        let binding: [u8; CHANNEL_BINDING_LEN] = *b"tls-channel-binding-material-32!";
        let mut handshake = test_handshake();

        handshake.psk_proof = compute_proof(b"correct-psk", &binding, &handshake);
        assert!(!verify_proof(b"wrong-psk", &binding, &handshake));
    }

    #[test]
    fn different_binding_rejected() {
        let binding_a: [u8; CHANNEL_BINDING_LEN] = *b"tls-channel-binding-material-0A!";
        let binding_b: [u8; CHANNEL_BINDING_LEN] = *b"tls-channel-binding-material-0B!";
        let mut handshake = test_handshake();

        handshake.psk_proof = compute_proof(b"psk", &binding_a, &handshake);
        assert!(!verify_proof(b"psk", &binding_b, &handshake));
    }

    #[test]
    fn different_handshakes_produce_different_proofs() {
        let psk = b"my-psk";
        let binding: [u8; CHANNEL_BINDING_LEN] = *b"tls-channel-binding-material-32!";

        let handshake1 = test_handshake();
        let mut handshake2 = test_handshake();
        handshake2.name = "other-node".to_string();

        let proof1 = compute_proof(psk, &binding, &handshake1);
        let proof2 = compute_proof(psk, &binding, &handshake2);
        assert_ne!(proof1, proof2);
    }

    #[test]
    fn serialization_is_deterministic() {
        let handshake = test_handshake();
        assert_eq!(
            handshake.serialize_for_proof(),
            handshake.serialize_for_proof(),
        );
    }

    #[test]
    fn serialization_includes_all_fields() {
        let mut handshake = test_handshake();
        let base = handshake.serialize_for_proof();

        handshake.capabilities.as_mut().unwrap().tun_capable = false;
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.capabilities.as_mut().unwrap().tun_capable = true;

        handshake.capabilities.as_mut().unwrap().listening = false;
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.capabilities.as_mut().unwrap().listening = true;

        handshake.capabilities.as_mut().unwrap().connecting = true;
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.capabilities.as_mut().unwrap().connecting = false;

        handshake.name = "changed".to_string();
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.name = "test-node".to_string();

        handshake.version = "0.2.0".to_string();
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.version = "0.1.0".to_string();

        handshake.routes = Vec::new();
        assert_ne!(handshake.serialize_for_proof(), base);
        handshake.routes = vec!["10.0.0.0/8".to_string()];

        handshake.hint = Some(wallhack_wire::data::RoleHint {
            level: 1,
            target: 1,
        });
        assert_ne!(handshake.serialize_for_proof(), base);
    }
}
