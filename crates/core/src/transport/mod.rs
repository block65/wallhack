//! Transport layer abstraction.
//!
//! Re-exports from the [`wallhack_transport`] crate and provides the application-level
//! [`protocol`] module for protobuf message routing over transports.

pub mod protocol;

pub use wallhack_transport::*;

/// Splice two bidirectional streams together, copying data in both directions
/// until either side closes. Calls `finish()` on both streams afterward to
/// signal a clean close (required for QUIC stream-level FIN semantics).
///
/// Used by the relay to bridge bidi streams between the source (entry) and
/// exit peer transports.
pub async fn splice_bi(mut a: BoxBiStream, mut b: BoxBiStream) -> Result<(), TransportError> {
    tokio::io::copy_bidirectional(&mut a, &mut b).await?;
    // Best-effort FIN — the remote may have already closed.
    let _ = a.finish().await;
    let _ = b.finish().await;
    Ok(())
}
