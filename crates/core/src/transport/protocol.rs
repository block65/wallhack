//! Transport protocol module.
//!
//! Provides generic async functions for bridging transport streams with broadcast channels.
//! This module extracts the common stream-handling logic from QUIC server/client implementations
//! to allow reuse with any [`Transport`] implementation.

use prost::Message;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::{EntryNodeInstruction, ExitNodeResponse, Handshake, TunnelMessage, tunnel_message},
};

use crate::control::handler::Handler;
use wallhack_transport::{Transport, TransportError, erased::BoxBiStream};

/// Maximum size for `TcpStreamHeader` and `TcpStreamStatus` messages (1KB).
pub const TCP_STREAM_HEADER_MTU: usize = 1024;

/// Maximum size for tunnel messages (2KB).
const TUNNEL_MTU: usize = 2000;

/// Maximum size for control messages (4KB).
pub const CONTROL_MTU: usize = 4096;

/// Extension trait for reading length-delimited protobuf messages.
pub trait AsyncProtoRead {
    /// Read a length-delimited protobuf message from this stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream closes unexpectedly or decoding fails.
    fn read_proto<M: Message + Default>(
        &mut self,
        max_len: usize,
    ) -> impl std::future::Future<Output = Result<M, TransportError>> + Send;
}

impl<S: tokio::io::AsyncRead + Unpin + Send> AsyncProtoRead for S {
    async fn read_proto<M: Message + Default>(
        &mut self,
        max_len: usize,
    ) -> Result<M, TransportError> {
        read_length_delimited_buf(self, max_len, &mut Vec::new()).await
    }
}

/// Extension trait for writing length-delimited protobuf messages.
pub trait AsyncProtoWrite {
    /// Write a length-delimited protobuf message to this stream.
    ///
    /// The `Sync` bound on `M` ensures that the returned future is `Send`
    /// (references to `M` cross an `.await` point).
    ///
    /// # Errors
    ///
    /// Returns an error if encoding or writing fails.
    fn write_proto<M: Message + Sync>(
        &mut self,
        msg: &M,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;
}

impl<S: tokio::io::AsyncWrite + Unpin + Send> AsyncProtoWrite for S {
    async fn write_proto<M: Message + Sync>(&mut self, msg: &M) -> Result<(), TransportError> {
        write_length_delimited_buf(self, msg, &mut Vec::new()).await
    }
}

/// Read a length-delimited protobuf from a read stream, reusing the provided buffer.
pub async fn read_length_delimited_buf<M: Message + Default, S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    max_len: usize,
    buf: &mut Vec<u8>,
) -> Result<M, TransportError> {
    let len = stream
        .read_u32()
        .await
        .map_err(|e| TransportError::stream(e.to_string()))?;
    let len = usize::try_from(len).map_err(|_| TransportError::stream("length overflow"))?;
    if len > max_len {
        return Err(TransportError::stream("length exceeds maximum"));
    }
    buf.clear();
    buf.resize(len, 0);
    stream
        .read_exact(buf)
        .await
        .map_err(|e| TransportError::stream(e.to_string()))?;
    M::decode(&buf[..]).map_err(|e| TransportError::stream(e.to_string()))
}

/// Write a length-delimited protobuf, reusing the provided buffer.
pub async fn write_length_delimited_buf<M: Message, S: tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    msg: &M,
    buf: &mut Vec<u8>,
) -> Result<(), TransportError> {
    buf.clear();
    msg.encode(buf)
        .map_err(|e| TransportError::stream(e.to_string()))?;
    let len = u32::try_from(buf.len()).map_err(|_| TransportError::stream("length overflow"))?;
    stream
        .write_u32(len)
        .await
        .map_err(|e| TransportError::stream(e.to_string()))?;
    stream
        .write_all(buf)
        .await
        .map_err(|e| TransportError::stream(e.to_string()))?;
    stream
        .flush()
        .await
        .map_err(|e| TransportError::stream(e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Two-channel architecture: persistent control bidi stream + data uni streams
// ---------------------------------------------------------------------------

/// Reason sent when the control loop shuts down.
#[derive(Debug)]
pub enum ControlLoopExit {
    /// Remote peer sent a Disconnect message.
    Disconnect(String),
    /// Stream was closed (EOF / transport error).
    StreamClosed,
}

/// Channels consumed by `ControlChannels::run`.
pub struct ControlChannels {
    /// Outgoing control messages injected by the caller.
    pub outgoing_rx: mpsc::Receiver<ControlMessage>,
    /// One-shot for the first `Handshake` received from the peer.
    pub handshake_tx: Option<oneshot::Sender<Handshake>>,
    /// Pong-derived latency measurements (milliseconds).
    pub latency_tx: Option<mpsc::Sender<f64>>,
    /// `ControlResponse` forwarding (client side, for correlating requests).
    pub control_response_tx: Option<mpsc::Sender<wallhack_wire::control::ControlResponse>>,
    /// `RoleTransition` forwarding to the mode task for re-evaluation.
    pub role_transition_tx: Option<mpsc::Sender<wallhack_wire::control::RoleTransition>>,
}

impl ControlChannels {
    /// Runs the persistent control bidi-stream loop.
    ///
    /// Multiplexes:
    /// - **Reading** `ControlMessage`s from the bidi stream and dispatching them.
    /// - **Writing** outgoing `ControlMessage`s injected via `self.outgoing_rx`.
    /// - **Ping timer** that periodically writes `Ping` messages.
    ///
    /// The `handler` is `Some` on the server side (to process incoming
    /// `ControlRequest`s) and `None` on the client side.
    pub async fn run(
        &mut self,
        stream: &mut BoxBiStream,
        handler: Option<&Handler>,
        ping_interval: std::time::Duration,
    ) -> ControlLoopExit {
        let mut read_buf = Vec::with_capacity(CONTROL_MTU);
        let mut write_buf = Vec::with_capacity(CONTROL_MTU);
        let mut ping_timer = tokio::time::interval(ping_interval);
        // Don't fire immediately — the first tick should be after the interval.
        ping_timer.reset();

        loop {
            tokio::select! {
                // Read incoming control messages
                result = read_length_delimited_buf::<ControlMessage, _>(
                    stream, CONTROL_MTU, &mut read_buf,
                ) => {
                    let msg = match result {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::trace!("Control stream ended: {e}");
                            return ControlLoopExit::StreamClosed;
                        }
                    };

                    if let Some(exit) = self.handle_message(stream, msg, handler, &mut write_buf).await {
                        return exit;
                    }
                }

                // Write outgoing control messages injected by the caller
                msg = self.outgoing_rx.recv() => {
                    let Some(msg) = msg else {
                        tracing::debug!("Control outgoing channel closed");
                        return ControlLoopExit::StreamClosed;
                    };
                    if let Err(e) = write_length_delimited_buf(stream, &msg, &mut write_buf).await {
                        tracing::warn!("Failed to write outgoing control message: {e}");
                        return ControlLoopExit::StreamClosed;
                    }
                }

                // Periodic ping
                _ = ping_timer.tick() => {
                    #[allow(clippy::cast_possible_truncation)] // millis since epoch fits u64 until ~year 584M
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let ping = ControlMessage {
                        message: Some(control_message::Message::Ping(
                            wallhack_wire::data::Ping { timestamp_ms: ts },
                        )),
                    };
                    if let Err(e) = write_length_delimited_buf(stream, &ping, &mut write_buf).await {
                        tracing::warn!("Failed to write Ping: {e}");
                        return ControlLoopExit::StreamClosed;
                    }
                }
            }
        }
    }

    /// Process a single incoming `ControlMessage`.
    ///
    /// Returns `Some(exit_reason)` if the loop should terminate.
    async fn handle_message(
        &mut self,
        stream: &mut BoxBiStream,
        msg: ControlMessage,
        handler: Option<&Handler>,
        write_buf: &mut Vec<u8>,
    ) -> Option<ControlLoopExit> {
        match msg.message {
            Some(control_message::Message::Handshake(hs)) => {
                tracing::info!("Handshake from {} ({})", hs.name, hs.version);
                if let Some(tx) = self.handshake_tx.take() {
                    let _ = tx.send(hs);
                }
            }
            Some(control_message::Message::Ping(ping_msg)) => {
                tracing::trace!("Control: received Ping, auto-replying Pong");
                let reply = ControlMessage {
                    message: Some(control_message::Message::Pong(wallhack_wire::data::Pong {
                        timestamp_ms: ping_msg.timestamp_ms,
                    })),
                };
                if let Err(e) = write_length_delimited_buf(stream, &reply, write_buf).await {
                    tracing::warn!("Failed to write Pong: {e}");
                    return Some(ControlLoopExit::StreamClosed);
                }
            }
            Some(control_message::Message::Pong(pong)) => {
                #[allow(clippy::cast_possible_truncation)]
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                #[allow(clippy::cast_precision_loss)]
                // ms-resolution latency; f64 mantissa exceeds plausible RTT range
                let latency_ms = now_ms.saturating_sub(pong.timestamp_ms) as f64;
                tracing::trace!(latency_ms, "Control: received Pong");
                if let Some(ref tx) = self.latency_tx {
                    let _ = tx.send(latency_ms).await;
                }
            }
            Some(control_message::Message::ControlRequest(req)) => {
                if let Some(h) = handler {
                    tracing::trace!("Control: handling ControlRequest");
                    let resp = h.handle(req);
                    let msg = ControlMessage {
                        message: Some(control_message::Message::ControlResponse(resp)),
                    };
                    if let Err(e) = write_length_delimited_buf(stream, &msg, write_buf).await {
                        tracing::warn!("Failed to write ControlResponse: {e}");
                        return Some(ControlLoopExit::StreamClosed);
                    }
                }
            }
            Some(control_message::Message::ControlResponse(resp)) => {
                tracing::trace!("Control: received ControlResponse");
                if let Some(ref tx) = self.control_response_tx {
                    let _ = tx.send(resp).await;
                }
            }
            Some(control_message::Message::Disconnect(dc)) => {
                tracing::info!("Control: received Disconnect: {}", dc.reason);
                return Some(ControlLoopExit::Disconnect(dc.reason));
            }
            Some(control_message::Message::RoleTransition(rt)) => {
                tracing::info!("Control: received RoleTransition: {:?}", rt.new_role());
                if let Some(ref tx) = self.role_transition_tx {
                    let _ = tx.send(rt).await;
                }
            }
            None => {
                tracing::warn!("Control: received empty ControlMessage");
            }
        }
        None
    }
}

/// Opens a bidi stream on the transport and runs the control loop (client side).
///
/// If `channels.handshake_tx` is provided, the first `Handshake` message
/// received on the control stream is forwarded through it — this is how the
/// client receives the server's handshake during the bidirectional exchange.
pub async fn run_control_stream_initiator<T: Transport>(
    transport: &T,
    channels: &mut ControlChannels,
    handler: Option<&Handler>,
    ping_interval: std::time::Duration,
) -> Result<ControlLoopExit, TransportError>
where
    T::BiStream: Send + 'static,
{
    let stream = transport.open_bi().await?;
    let mut stream = BoxBiStream::new(stream);
    Ok(channels.run(&mut stream, handler, ping_interval).await)
}

/// Accepts the first bidi stream on the transport and runs the control loop (server side).
///
/// The first `ControlMessage` on the accepted stream MUST be a `Handshake`.
/// The `channels.handshake_tx` oneshot is provided so the caller can receive
/// the handshake before deciding whether to spawn data tasks.
pub async fn run_control_stream_acceptor<T: Transport>(
    transport: &T,
    channels: &mut ControlChannels,
    handler: Option<&Handler>,
    ping_interval: std::time::Duration,
) -> Result<ControlLoopExit, TransportError>
where
    T::BiStream: Send + 'static,
{
    let Some(stream) = transport.accept_bi().await? else {
        return Ok(ControlLoopExit::StreamClosed);
    };
    let mut stream = BoxBiStream::new(stream);
    Ok(channels.run(&mut stream, handler, ping_interval).await)
}

/// Reads `TunnelMessage`s from a receive stream, dispatching only data
/// messages (instructions and responses) to mpsc channels.
///
/// Unlike `run_incoming_data`, this function does NOT handle Handshake,
/// Ping, or Pong — those are handled on the control stream.
pub async fn run_data_in<S: tokio::io::AsyncRead + Unpin>(
    recv: &mut S,
    instructions_tx: &mpsc::Sender<EntryNodeInstruction>,
    responses_tx: &mpsc::Sender<ExitNodeResponse>,
) -> Result<(), TransportError> {
    let mut read_buf = Vec::with_capacity(TUNNEL_MTU);
    loop {
        let msg: TunnelMessage =
            match read_length_delimited_buf(recv, TUNNEL_MTU, &mut read_buf).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::trace!("Data-in stream ended: {e}");
                    return Err(e);
                }
            };

        match msg.message {
            Some(tunnel_message::Message::EntryNodeInstruction(instr)) => {
                if instructions_tx.send(instr).await.is_err() {
                    tracing::debug!("Instructions receiver closed, stopping data-in");
                    return Ok(());
                }
            }
            Some(tunnel_message::Message::ExitNodeResponse(resp)) => {
                if responses_tx.send(resp).await.is_err() {
                    tracing::debug!("Responses receiver closed, stopping data-in");
                    return Ok(());
                }
            }
            Some(tunnel_message::Message::RawPacket(pkt)) => {
                tracing::warn!("Unhandled RawPacket: {} bytes", pkt.data.len());
            }
            other => {
                tracing::warn!("Unexpected message on data stream: {other:?}");
            }
        }
    }
}

/// Reads `EntryNodeInstruction`s from an mpsc receiver and writes them as
/// `TunnelMessage`s on a send stream.
pub async fn run_send_instructions<S: tokio::io::AsyncWrite + Unpin>(
    send: &mut S,
    mut rx: mpsc::Receiver<EntryNodeInstruction>,
) -> Result<(), TransportError> {
    let mut buf = Vec::with_capacity(TUNNEL_MTU);

    loop {
        let Some(instruction) = rx.recv().await else {
            tracing::debug!("Instructions channel closed");
            return Ok(());
        };

        let tunnel_msg = TunnelMessage::from(instruction);
        if let Err(e) = write_length_delimited_buf(send, &tunnel_msg, &mut buf).await {
            tracing::error!("Failed to write instruction: {e}");
            return Err(e);
        }
    }
}

/// Reads `ExitNodeResponse`s from an mpsc receiver and writes them as
/// `TunnelMessage`s on a send stream.
pub async fn run_send_responses<S: tokio::io::AsyncWrite + Unpin>(
    send: &mut S,
    mut rx: mpsc::Receiver<ExitNodeResponse>,
) -> Result<(), TransportError> {
    let mut buf = Vec::with_capacity(TUNNEL_MTU);

    loop {
        let Some(response) = rx.recv().await else {
            tracing::debug!("Responses channel closed");
            return Ok(());
        };

        tracing::debug!(
            response_type = ?response.response.as_ref().map(std::mem::discriminant),
            "Sending response to peer"
        );
        let tunnel_msg = TunnelMessage::from(response);
        if let Err(e) = write_length_delimited_buf(send, &tunnel_msg, &mut buf).await {
            tracing::error!("Failed to write response: {e}");
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::{
        io::{DuplexStream, duplex},
        sync::mpsc as tokio_mpsc,
    };

    /// A minimal mock transport for testing protocol functions.
    ///
    /// Uses `tokio::io::duplex` streams routed through mpsc channels to simulate
    /// a multiplexed transport. Each `open_uni()` on one side creates a duplex
    /// pair and sends the read half to the other side's `accept_uni()`.
    struct MockTransport {
        outgoing_tx: tokio_mpsc::UnboundedSender<DuplexStream>,
        incoming_rx: tokio::sync::Mutex<tokio_mpsc::UnboundedReceiver<DuplexStream>>,
    }

    impl MockTransport {
        fn pair() -> (Self, Self) {
            let (a_tx, a_rx) = tokio_mpsc::unbounded_channel();
            let (b_tx, b_rx) = tokio_mpsc::unbounded_channel();
            (
                Self {
                    outgoing_tx: b_tx,
                    incoming_rx: tokio::sync::Mutex::new(a_rx),
                },
                Self {
                    outgoing_tx: a_tx,
                    incoming_rx: tokio::sync::Mutex::new(b_rx),
                },
            )
        }
    }

    struct MockBiStream(DuplexStream);

    impl tokio::io::AsyncRead for MockBiStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl tokio::io::AsyncWrite for MockBiStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    impl wallhack_transport::BiStream for MockBiStream {
        async fn finish(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    impl wallhack_transport::Transport for MockTransport {
        type SendStream = DuplexStream;
        type RecvStream = DuplexStream;
        type BiStream = MockBiStream;

        async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
            let (writer, reader) = duplex(64 * 1024);
            self.outgoing_tx
                .send(reader)
                .map_err(|_| TransportError::stream("peer closed"))?;
            Ok(writer)
        }
        async fn open_bi(&self) -> Result<Self::BiStream, TransportError> {
            Err(TransportError::stream("not implemented"))
        }
        async fn accept_uni(&self) -> Result<Option<Self::RecvStream>, TransportError> {
            let mut rx = self.incoming_rx.lock().await;
            Ok(rx.recv().await)
        }
        async fn accept_bi(&self) -> Result<Option<Self::BiStream>, TransportError> {
            Err(TransportError::stream("not implemented"))
        }
        async fn close(&self) -> Result<(), TransportError> {
            Ok(())
        }
        fn remote_addr(&self) -> Option<SocketAddr> {
            None
        }
    }

    /// Test that `run_data_in` correctly dispatches data messages on a
    /// uni stream using length-delimited framing.
    #[tokio::test]
    async fn test_data_in_dispatches_responses() {
        let (sender, receiver) = MockTransport::pair();

        let (instructions_tx, _instructions_rx) = tokio_mpsc::channel::<EntryNodeInstruction>(16);
        let (responses_tx, mut responses_rx) = tokio_mpsc::channel::<ExitNodeResponse>(16);

        let recv_handle = tokio::spawn(async move {
            match receiver.accept_uni().await {
                Ok(Some(mut recv)) => run_data_in(&mut recv, &instructions_tx, &responses_tx).await,
                _ => panic!("expected uni stream"),
            }
        });

        // Send multiple responses on one persistent stream.
        let mut send = sender.open_uni().await.unwrap();
        let mut buf = Vec::new();
        for _ in 0..3 {
            let msg = TunnelMessage::from(ExitNodeResponse::default());
            write_length_delimited_buf(&mut send, &msg, &mut buf)
                .await
                .unwrap();
        }

        for _ in 0..3 {
            tokio::time::timeout(std::time::Duration::from_secs(2), responses_rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
        }

        drop(sender);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), recv_handle).await;
    }

    /// End-to-end: `run_send_responses` → transport → `run_data_in`.
    #[tokio::test]
    async fn test_data_out_to_data_in_roundtrip() {
        let (exit_transport, entry_transport) = MockTransport::pair();

        let (responses_src_tx, responses_src_rx) = tokio_mpsc::channel::<ExitNodeResponse>(16);
        let (instructions_dst_tx, _instructions_dst_rx) =
            tokio_mpsc::channel::<EntryNodeInstruction>(16);
        let (responses_dst_tx, mut responses_dst_rx) = tokio_mpsc::channel::<ExitNodeResponse>(16);

        let outgoing = tokio::spawn({
            async move {
                match exit_transport.open_uni().await {
                    Ok(mut send) => run_send_responses(&mut send, responses_src_rx).await,
                    Err(e) => Err(e),
                }
            }
        });

        let incoming = tokio::spawn(async move {
            match entry_transport.accept_uni().await {
                Ok(Some(mut recv)) => {
                    run_data_in(&mut recv, &instructions_dst_tx, &responses_dst_tx).await
                }
                _ => panic!("expected uni stream"),
            }
        });

        for _ in 0..3 {
            responses_src_tx
                .send(ExitNodeResponse::default())
                .await
                .unwrap();
        }

        for _ in 0..3 {
            tokio::time::timeout(std::time::Duration::from_secs(2), responses_dst_rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
        }

        drop(responses_src_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), outgoing).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), incoming).await;
    }

    /// Helper: create a connected pair of `MockBiStream`s for control loop testing.
    fn bidi_pair() -> (MockBiStream, MockBiStream) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        (MockBiStream(a), MockBiStream(b))
    }

    /// Both sides send Handshake concurrently and each receives the other's.
    #[tokio::test]
    async fn test_handshake_exchange() {
        use wallhack_wire::data::Handshake;

        let (stream_a, stream_b) = bidi_pair();

        // Side A: sends its handshake, receives B's.
        let (a_hs_tx, a_hs_rx) = tokio::sync::oneshot::channel::<Handshake>();
        let (a_ctrl_tx, a_ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);
        let hs_a = Handshake {
            name: "node-a".into(),
            version: "1.0".into(),
            ..Default::default()
        };
        a_ctrl_tx
            .send(ControlMessage {
                message: Some(control_message::Message::Handshake(hs_a)),
            })
            .await
            .unwrap();

        // Side B: sends its handshake, receives A's.
        let (b_hs_tx, b_hs_rx) = tokio::sync::oneshot::channel::<Handshake>();
        let (b_ctrl_tx, b_ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);
        let hs_b = Handshake {
            name: "node-b".into(),
            version: "2.0".into(),
            ..Default::default()
        };
        b_ctrl_tx
            .send(ControlMessage {
                message: Some(control_message::Message::Handshake(hs_b)),
            })
            .await
            .unwrap();

        let a_handle = tokio::spawn(async move {
            let mut channels = ControlChannels {
                outgoing_rx: a_ctrl_rx,
                handshake_tx: Some(a_hs_tx),
                latency_tx: None,
                control_response_tx: None,
                role_transition_tx: None,
            };
            let mut stream_a = BoxBiStream::new(stream_a);
            channels
                .run(&mut stream_a, None, std::time::Duration::from_mins(10))
                .await
        });

        let b_handle = tokio::spawn(async move {
            let mut channels = ControlChannels {
                outgoing_rx: b_ctrl_rx,
                handshake_tx: Some(b_hs_tx),
                latency_tx: None,
                control_response_tx: None,
                role_transition_tx: None,
            };
            let mut stream_b = BoxBiStream::new(stream_b);
            channels
                .run(&mut stream_b, None, std::time::Duration::from_mins(10))
                .await
        });

        // A should receive B's handshake and vice versa.
        let received_by_a = tokio::time::timeout(std::time::Duration::from_secs(2), a_hs_rx)
            .await
            .expect("timed out")
            .expect("oneshot closed");
        assert_eq!(received_by_a.name, "node-b");
        assert_eq!(received_by_a.version, "2.0");

        let received_by_b = tokio::time::timeout(std::time::Duration::from_secs(2), b_hs_rx)
            .await
            .expect("timed out")
            .expect("oneshot closed");
        assert_eq!(received_by_b.name, "node-a");
        assert_eq!(received_by_b.version, "1.0");

        // Clean up
        a_handle.abort();
        b_handle.abort();
    }

    /// Control loop rejects a malformed handshake (non-Handshake first message
    /// is ignored; `handshake_tx` is never fulfilled).
    #[tokio::test]
    async fn test_malformed_handshake() {
        let (mut stream_a, stream_b) = bidi_pair();

        // Send a Ping instead of a Handshake as the first message.
        let bad_msg = ControlMessage {
            message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
                timestamp_ms: 0,
            })),
        };
        let mut buf = Vec::new();
        write_length_delimited_buf(&mut stream_a, &bad_msg, &mut buf)
            .await
            .unwrap();
        drop(stream_a); // close the stream after sending

        let (hs_tx, mut hs_rx) = tokio::sync::oneshot::channel::<Handshake>();
        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);

        let mut channels = ControlChannels {
            outgoing_rx: ctrl_rx,
            handshake_tx: Some(hs_tx),
            latency_tx: None,
            control_response_tx: None,
            role_transition_tx: None,
        };

        let mut stream_b = BoxBiStream::new(stream_b);
        let exit = channels
            .run(&mut stream_b, None, std::time::Duration::from_mins(10))
            .await;

        // Stream closed after the bad message.
        assert!(matches!(exit, ControlLoopExit::StreamClosed));
        // Handshake was never delivered.
        assert!(hs_rx.try_recv().is_err());
    }

    /// Pong latency is computed and forwarded via `latency_tx`.
    #[tokio::test]
    async fn test_ping_latency() {
        let (mut stream_a, stream_b) = bidi_pair();

        let (latency_tx, mut latency_rx) = tokio::sync::mpsc::channel::<f64>(4);
        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);

        let mut channels = ControlChannels {
            outgoing_rx: ctrl_rx,
            handshake_tx: None,
            latency_tx: Some(latency_tx),
            control_response_tx: None,
            role_transition_tx: None,
        };

        // Spawn the control loop on side B (will read from stream_b).
        let b_handle = tokio::spawn(async move {
            let mut stream_b = BoxBiStream::new(stream_b);
            channels
                .run(&mut stream_b, None, std::time::Duration::from_mins(10))
                .await
        });

        // First, verify Ping auto-reply: send a Ping, read the Pong.
        #[allow(clippy::cast_possible_truncation)]
        let ping_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let outgoing_ping = ControlMessage {
            message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
                timestamp_ms: ping_ts,
            })),
        };
        let mut buf = Vec::new();
        write_length_delimited_buf(&mut stream_a, &outgoing_ping, &mut buf)
            .await
            .unwrap();

        // The control loop auto-replies Pong. Read it from stream A.
        let pong: ControlMessage = stream_a
            .read_proto::<ControlMessage>(CONTROL_MTU)
            .await
            .unwrap();
        match pong.message {
            Some(control_message::Message::Pong(p)) => {
                assert_eq!(p.timestamp_ms, ping_ts);
            }
            other => panic!("expected Pong, got: {other:?}"),
        }

        // Now send a Pong with a timestamp 100ms in the past to test latency
        // computation. The control loop uses SystemTime, so we subtract from now.
        #[allow(clippy::cast_possible_truncation)]
        let past_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 100;

        let incoming_pong = ControlMessage {
            message: Some(control_message::Message::Pong(wallhack_wire::data::Pong {
                timestamp_ms: past_ts,
            })),
        };
        write_length_delimited_buf(&mut stream_a, &incoming_pong, &mut buf)
            .await
            .unwrap();

        // The control loop should forward the latency via latency_tx.
        let ms = tokio::time::timeout(std::time::Duration::from_secs(2), latency_rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        // Latency should be approximately 100ms (within ±50ms tolerance for CI).
        assert!((50.0..=200.0).contains(&ms), "expected ~100ms, got {ms}ms");

        drop(stream_a);
        let _ = b_handle.await;
    }

    /// Periodic ping timer fires at the configured interval.
    #[tokio::test(start_paused = true)]
    async fn test_periodic_ping() {
        let (mut stream_a, stream_b) = bidi_pair();

        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);
        let mut channels = ControlChannels {
            outgoing_rx: ctrl_rx,
            handshake_tx: None,
            latency_tx: None,
            control_response_tx: None,
            role_transition_tx: None,
        };

        // Control loop with 1-second ping interval.
        let b_handle = tokio::spawn(async move {
            let mut stream_b = BoxBiStream::new(stream_b);
            channels
                .run(&mut stream_b, None, std::time::Duration::from_secs(1))
                .await
        });

        // Advance time past the first ping interval.
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        // Yield to let the timer fire.
        tokio::task::yield_now().await;

        // Read the Ping from stream A.
        let msg: ControlMessage = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream_a.read_proto::<ControlMessage>(CONTROL_MTU),
        )
        .await
        .expect("timed out")
        .expect("read error");

        match msg.message {
            Some(control_message::Message::Ping(p)) => {
                assert!(p.timestamp_ms > 0, "ping timestamp should be non-zero");
            }
            other => panic!("expected Ping, got: {other:?}"),
        }

        drop(stream_a);
        let _ = b_handle.await;
    }

    /// Control plane continues normally when handler role is Indeterminate.
    ///
    /// Sends a Ping to the server-side control loop, which has a Handler
    /// configured with `NodeRole::Indeterminate`, and verifies the auto-reply
    /// Pong arrives — proving the control loop keeps running.
    #[tokio::test]
    async fn test_control_plane_indeterminate() {
        use crate::{
            NodeRole,
            control::{
                handler::{Handler, HandlerConfig},
                metrics::Metrics,
                peers::Registry,
                routes::RouteTable,
            },
        };

        let (mut client_stream, server_stream) = bidi_pair();

        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Indeterminate,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            std::sync::Arc::new(Metrics::default()),
            std::sync::Arc::new(Registry::new()),
            RouteTable::shared(),
        );

        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);
        let mut channels = ControlChannels {
            outgoing_rx: ctrl_rx,
            handshake_tx: None,
            latency_tx: None,
            control_response_tx: None,
            role_transition_tx: None,
        };

        let server_handle = tokio::spawn(async move {
            let mut server_stream = BoxBiStream::new(server_stream);
            channels
                .run(
                    &mut server_stream,
                    Some(&handler),
                    std::time::Duration::from_mins(10),
                )
                .await
        });

        // Send Ping, expect Pong back.
        let outgoing = ControlMessage {
            message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
                timestamp_ms: 42,
            })),
        };
        let mut buf = Vec::new();
        write_length_delimited_buf(&mut client_stream, &outgoing, &mut buf)
            .await
            .unwrap();

        let reply: ControlMessage = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client_stream.read_proto::<ControlMessage>(CONTROL_MTU),
        )
        .await
        .expect("timed out")
        .expect("read error");

        match reply.message {
            Some(control_message::Message::Pong(p)) => {
                assert_eq!(p.timestamp_ms, 42);
            }
            other => panic!("expected Pong, got: {other:?}"),
        }

        drop(client_stream);
        let _ = server_handle.await;
    }

    /// Data plane is paused for `NodeRole::Indeterminate`: the outgoing data
    /// task completes immediately without opening a uni stream, so no data
    /// is sent even though the channel has messages.
    #[tokio::test]
    async fn test_data_plane_paused_indeterminate() {
        use crate::NodeRole;

        let (transport_a, transport_b) = MockTransport::pair();
        let (_responses_tx, responses_rx) = tokio_mpsc::channel::<ExitNodeResponse>(16);
        let (_instructions_tx, instructions_rx) = tokio_mpsc::channel::<EntryNodeInstruction>(16);

        // Simulate the Indeterminate arm from client/quic and client/ws:
        // spawns a no-op future instead of opening a data stream.
        let role = NodeRole::Indeterminate;
        let outgoing_handle = match role {
            NodeRole::Indeterminate => tokio::spawn(std::future::ready(())),
            NodeRole::Entry | NodeRole::Relay => tokio::spawn(async move {
                match transport_a.open_uni().await {
                    Ok(mut send) => {
                        let _ = run_send_instructions(&mut send, instructions_rx).await;
                    }
                    Err(e) => panic!("open_uni failed: {e}"),
                }
            }),
            NodeRole::Exit => tokio::spawn(async move {
                match transport_a.open_uni().await {
                    Ok(mut send) => {
                        let _ = run_send_responses(&mut send, responses_rx).await;
                    }
                    Err(e) => panic!("open_uni failed: {e}"),
                }
            }),
        };

        // The outgoing task should complete immediately.
        tokio::time::timeout(std::time::Duration::from_secs(1), outgoing_handle)
            .await
            .expect("outgoing task should complete immediately for Indeterminate")
            .expect("task panicked");

        // No uni stream should have been opened on the other side.
        let accept_result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            transport_b.accept_uni(),
        )
        .await;
        assert!(
            accept_result.is_err(),
            "no data stream should be opened in Indeterminate mode"
        );
    }

    /// Transport connection survives with an Indeterminate handler: multiple
    /// sequential ping/pong exchanges succeed, proving the connection stays
    /// open and the control loop remains responsive.
    #[tokio::test]
    async fn test_transport_survives_indeterminate() {
        use crate::{
            NodeRole,
            control::{
                handler::{Handler, HandlerConfig},
                metrics::Metrics,
                peers::Registry,
                routes::RouteTable,
            },
        };

        let (mut client_stream, server_stream) = bidi_pair();

        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Indeterminate,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            std::sync::Arc::new(Metrics::default()),
            std::sync::Arc::new(Registry::new()),
            RouteTable::shared(),
        );

        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlMessage>(16);
        let mut channels = ControlChannels {
            outgoing_rx: ctrl_rx,
            handshake_tx: None,
            latency_tx: None,
            control_response_tx: None,
            role_transition_tx: None,
        };

        let server_handle = tokio::spawn(async move {
            let mut server_stream = BoxBiStream::new(server_stream);
            channels
                .run(
                    &mut server_stream,
                    Some(&handler),
                    std::time::Duration::from_mins(10),
                )
                .await
        });

        // Send multiple pings over the same connection to prove it stays open.
        let mut buf = Vec::new();
        for seq in 0..5_u64 {
            let outgoing = ControlMessage {
                message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
                    timestamp_ms: seq,
                })),
            };
            write_length_delimited_buf(&mut client_stream, &outgoing, &mut buf)
                .await
                .unwrap();

            let reply: ControlMessage = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client_stream.read_proto::<ControlMessage>(CONTROL_MTU),
            )
            .await
            .expect("timed out — connection did not survive")
            .expect("read error");

            match reply.message {
                Some(control_message::Message::Pong(p)) => {
                    assert_eq!(p.timestamp_ms, seq);
                }
                other => panic!("expected Pong #{seq}, got: {other:?}"),
            }
        }

        drop(client_stream);
        let _ = server_handle.await;
    }
}
