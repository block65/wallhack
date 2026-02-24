//! Length-delimited protobuf framing over byte streams.
//!
//! Local copy of the framing helpers from `wallhack-core::transport::bridge`,
//! keeping this crate free of heavy transport dependencies.

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Read a length-delimited protobuf message from the stream.
///
/// Wire format: `[u32 big-endian length][protobuf bytes]`.
///
/// # Errors
///
/// Returns an error if the stream closes, the length exceeds `max_len`,
/// or protobuf decoding fails.
pub async fn read_length_delimited<M: Message + Default>(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    max_len: usize,
) -> std::io::Result<M> {
    let len = stream.read_u32().await? as usize;
    if len > max_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message length exceeds maximum",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    M::decode(&buf[..]).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write a length-delimited protobuf message to the stream.
///
/// Wire format: `[u32 big-endian length][protobuf bytes]`.
///
/// # Errors
///
/// Returns an error if encoding or writing fails.
pub async fn write_length_delimited<M: Message>(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    msg: &M,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    msg.encode(&mut buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(buf.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "length overflow"))?;
    stream.write_u32(len).await?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}
