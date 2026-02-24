//! WebSocket byte stream adapter.
//!
//! Adapts a message-framed [`WebSocketStream`] into a byte-oriented
//! [`AsyncRead`] + [`AsyncWrite`] stream suitable for yamux multiplexing.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{sink::Sink, stream::Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::tungstenite::Message;

/// A byte stream adapter over a WebSocket connection.
///
/// This adapter converts the message-based WebSocket protocol into a continuous
/// byte stream suitable for use with stream multiplexers like yamux.
///
/// # Implementation Notes
///
/// - Reads buffer partial message data for consumption across multiple read
///   calls
/// - Writes send binary messages for each write operation
/// - Only binary WebSocket messages are used for data; text/ping/pong/close are
///   handled separately
pub struct WebSocketByteStream<S> {
    inner: S,
    /// Unconsumed bytes from the tail of the last binary message.
    /// Stored as [`Bytes`] so partial-read overflow is a zero-copy slice.
    read_buf: Bytes,
}

impl<S> WebSocketByteStream<S> {
    /// Creates a new byte stream adapter over the given WebSocket stream.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            read_buf: Bytes::new(),
        }
    }

    /// Returns a reference to the underlying WebSocket stream.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Returns a mutable reference to the underlying WebSocket stream.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Consumes this adapter and returns the underlying WebSocket stream.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S> AsyncRead for WebSocketByteStream<S>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Drain any leftover bytes from a previous message first.
        if !self.read_buf.is_empty() {
            let to_copy = self.read_buf.len().min(buf.remaining());
            buf.put_slice(&self.read_buf[..to_copy]);
            self.read_buf = self.read_buf.slice(to_copy..);
            return Poll::Ready(Ok(()));
        }

        // Buffer is empty, read the next WebSocket message
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(msg))) => {
                    match msg {
                        Message::Binary(data) => {
                            if data.is_empty() {
                                continue;
                            }

                            let to_copy = data.len().min(buf.remaining());
                            buf.put_slice(&data[..to_copy]);

                            // Buffer the tail — zero-copy slice of the existing Bytes.
                            if to_copy < data.len() {
                                self.read_buf = data.slice(to_copy..);
                            }

                            return Poll::Ready(Ok(()));
                        }
                        Message::Close(_) => {
                            // Connection closed
                            return Poll::Ready(Ok(()));
                        }
                        Message::Ping(_)
                        | Message::Pong(_)
                        | Message::Text(_)
                        | Message::Frame(_) => {}
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::other(e)));
                }
                Poll::Ready(None) => {
                    // Stream ended
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

impl<S> AsyncWrite for WebSocketByteStream<S>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // First ensure the sink is ready to receive
        match Pin::new(&mut self.inner).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::other(e)));
            }
            Poll::Pending => {
                return Poll::Pending;
            }
        }

        // Send the data as a binary message
        let msg = Message::Binary(Bytes::copy_from_slice(buf));
        match Pin::new(&mut self.inner).start_send(msg) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(e) => Poll::Ready(Err(io::Error::other(e))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Send a close message
        match Pin::new(&mut self.inner).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::other(e)));
            }
            Poll::Pending => {
                return Poll::Pending;
            }
        }

        if let Err(e) = Pin::new(&mut self.inner).start_send(Message::Close(None)) {
            return Poll::Ready(Err(io::Error::other(e)));
        }

        Pin::new(&mut self.inner)
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        pin::Pin,
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use futures::{Sink, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::Message;

    use super::WebSocketByteStream;

    struct MockWebSocket {
        inbound: VecDeque<Message>,
        outbound: Vec<Message>,
    }

    impl MockWebSocket {
        fn with_messages(msgs: impl IntoIterator<Item = Message>) -> Self {
            Self {
                inbound: msgs.into_iter().collect(),
                outbound: Vec::new(),
            }
        }
    }

    impl Unpin for MockWebSocket {}

    impl Stream for MockWebSocket {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.inbound.pop_front().map(Ok))
        }
    }

    impl Sink<Message> for MockWebSocket {
        type Error = tokio_tungstenite::tungstenite::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.outbound.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_partial_reads() {
        // A 256-byte message split across 64-byte reads.
        let data = vec![0xABu8; 256];
        let mock = MockWebSocket::with_messages([Message::Binary(Bytes::from(data.clone()))]);
        let mut stream = WebSocketByteStream::new(mock);

        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 64);
        assert_eq!(&buf[..n], &data[..64]);

        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 64);
        assert_eq!(&buf[..n], &data[64..128]);

        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, &data[128..]);
    }

    #[tokio::test]
    async fn test_empty_messages_skipped() {
        // Empty binary frames must be skipped; data arrives from the next message.
        let mock = MockWebSocket::with_messages([
            Message::Binary(Bytes::new()),
            Message::Binary(Bytes::new()),
            Message::Binary(Bytes::from_static(b"hello")),
        ]);
        let mut stream = WebSocketByteStream::new(mock);

        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[tokio::test]
    async fn test_non_binary_messages_skipped() {
        // Ping/pong/text frames must be silently ignored.
        let mock = MockWebSocket::with_messages([
            Message::Ping(Bytes::new()),
            Message::Pong(Bytes::new()),
            Message::Text(String::new().into()),
            Message::Binary(Bytes::from_static(b"data")),
        ]);
        let mut stream = WebSocketByteStream::new(mock);

        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[tokio::test]
    async fn test_large_write() {
        // A write larger than a typical WebSocket frame is sent as a single binary message.
        let data = vec![0xABu8; 65_536];
        let mut stream = WebSocketByteStream::new(MockWebSocket::with_messages([]));

        let n = stream.write(&data).await.unwrap();
        assert_eq!(n, data.len());
        stream.flush().await.unwrap();

        let inner = stream.into_inner();
        assert_eq!(inner.outbound.len(), 1);
        match &inner.outbound[0] {
            Message::Binary(b) => assert_eq!(b.as_ref(), data.as_slice()),
            other => panic!("expected Binary message, got {other:?}"),
        }
    }
}
