//! Generic HMAC-SHA256 proof computation and verification.
//!
//! Transport-agnostic functions that produce and verify HMAC-SHA256 tags
//! over an arbitrary `(context, message)` pair.

use ring::hmac;

/// Compute `HMAC-SHA256(secret, context || message)`.
///
/// `context` is prepended to `message` before signing — callers typically
/// pass session-specific material (e.g. exported keying material) so that
/// the resulting tag is bound to a particular session.
#[must_use]
pub fn compute(secret: &[u8], context: &[u8], message: &[u8]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let mut data = Vec::with_capacity(context.len() + message.len());
    data.extend_from_slice(context);
    data.extend_from_slice(message);
    hmac::sign(&key, &data).as_ref().to_vec()
}

/// Verify an HMAC-SHA256 tag (constant-time comparison via `ring`).
///
/// Returns `true` if `tag` matches `HMAC-SHA256(secret, context || message)`.
#[must_use]
pub fn verify(secret: &[u8], context: &[u8], message: &[u8], tag: &[u8]) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let mut data = Vec::with_capacity(context.len() + message.len());
    data.extend_from_slice(context);
    data.extend_from_slice(message);
    hmac::verify(&key, &data, tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let secret = b"shared-secret";
        let context = b"some-context-bytes-for-the-test!";
        let message = b"some-authenticated-message";

        let tag = compute(secret, context, message);
        assert_eq!(tag.len(), 32);
        assert!(verify(secret, context, message, &tag));
    }

    #[test]
    fn known_output() {
        // HMAC-SHA256(key="key", data="contextmessage")
        // ring produces the standard HMAC-SHA256 output for this input.
        let tag = compute(b"key", b"context", b"message");
        let expected: [u8; 32] = [
            0x63, 0xf7, 0x22, 0x09, 0x6c, 0x60, 0xad, 0x63, 0x3b, 0x32, 0xe6, 0x37, 0xc1, 0xa3,
            0x37, 0x1d, 0x00, 0xc4, 0x9e, 0xcb, 0xb2, 0x86, 0x1c, 0xfe, 0x31, 0x5c, 0x3a, 0x4c,
            0x60, 0xf4, 0xbe, 0x25,
        ];
        assert_eq!(tag.as_slice(), &expected);
    }

    #[test]
    fn wrong_secret_rejected() {
        let context = b"ctx";
        let message = b"message";

        let tag = compute(b"correct", context, message);
        assert!(!verify(b"wrong", context, message, &tag));
    }

    #[test]
    fn wrong_context_rejected() {
        let secret = b"secret";
        let message = b"message";

        let tag = compute(secret, b"context-A", message);
        assert!(!verify(secret, b"context-B", message, &tag));
    }

    #[test]
    fn wrong_message_rejected() {
        let secret = b"secret";
        let context = b"ctx";

        let tag = compute(secret, context, b"message-A");
        assert!(!verify(secret, context, b"message-B", &tag));
    }

    #[test]
    fn empty_tag_rejected() {
        assert!(!verify(b"secret", b"ctx", b"message", &[]));
    }

    #[test]
    fn truncated_tag_rejected() {
        let tag = compute(b"secret", b"ctx", b"message");
        assert!(!verify(b"secret", b"ctx", b"message", &tag[..16]));
    }
}
