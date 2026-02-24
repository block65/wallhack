//! Transport bridge module.
//!
//! Provides generic async functions for bridging transport streams with broadcast channels.
//! This module extracts the common stream-handling logic from QUIC server/client implementations
//! to allow reuse with any [`Transport`] implementation.

use prost::Message;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{broadcast, mpsc, oneshot},
};
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse, TunnelMessage, tunnel_message},
};

use crate::control::handler::Handler;
use wallhack_transport::{BiStream, Transport, TransportError};

/// Maximum size for session init messages (1KB).
pub const SESSION_INIT_MTU: usize = 1024;

/// Maximum size for tunnel messages (2KB).
const TUNNEL_MTU: usize = 2000;

/// Maximum size for control messages (4KB).
pub const CONTROL_MTU: usize = 4096;

/// Read a length-delimited protobuf from the stream.
///
/// # Errors
///
/// Returns an error if the stream closes unexpectedly or decoding fails.
pub async fn read_length_delimited<M: Message + Default, S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    max_len: usize,
) -> Result<M, TransportError> {
    read_length_delimited_buf(stream, max_len, &mut Vec::new()).await
}

/// Read a length-delimited protobuf from the stream, reusing the provided buffer.
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

/// Write a length-delimited protobuf to the stream.
///
/// Uses a caller-provided buffer to avoid per-call allocation. Falls back to
/// an internal buffer when `None` is passed.
///
/// # Errors
///
/// Returns an error if encoding or writing fails.
pub async fn write_length_delimited<M: Message, S: tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    msg: &M,
) -> Result<(), TransportError> {
    write_length_delimited_buf(stream, msg, &mut Vec::new()).await
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

/// Runs the persistent control bidi-stream loop.
///
/// Multiplexes:
/// - **Reading** `ControlMessage`s from the bidi stream and dispatching them.
/// - **Writing** outgoing `ControlMessage`s injected via `outgoing_rx`.
/// - **Ping timer** that periodically writes `Ping` messages.
///
/// The `handler` is `Some` on the server side (to process incoming
/// `ControlRequest`s) and `None` on the client side.
#[allow(clippy::too_many_lines)] // refactor candidate
pub async fn run_control_loop<S: BiStream>(
    stream: &mut S,
    outgoing_rx: &mut mpsc::Receiver<ControlMessage>,
    handler: Option<&Handler>,
    hello_tx: Option<oneshot::Sender<ExitNodeHello>>,
    pong_tx: Option<&mpsc::Sender<wallhack_wire::data::Pong>>,
    control_response_tx: Option<&mpsc::Sender<wallhack_wire::control::ControlResponse>>,
    ping_interval: std::time::Duration,
) -> ControlLoopExit {
    let mut read_buf = Vec::with_capacity(CONTROL_MTU);
    let mut write_buf = Vec::with_capacity(CONTROL_MTU);
    let mut hello_tx = hello_tx;
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

                match msg.message {
                    Some(control_message::Message::Hello(hello)) => {
                        tracing::info!(
                            "Control: received Hello name={} version={}",
                            hello.name, hello.version,
                        );
                        if let Some(tx) = hello_tx.take() {
                            let _ = tx.send(hello);
                        }
                    }
                    Some(control_message::Message::Ping(ping_msg)) => {
                        tracing::trace!("Control: received Ping, auto-replying Pong");
                        let reply = ControlMessage {
                            message: Some(control_message::Message::Pong(
                                wallhack_wire::data::Pong { timestamp_ms: ping_msg.timestamp_ms },
                            )),
                        };
                        if let Err(e) = write_length_delimited_buf(stream, &reply, &mut write_buf).await {
                            tracing::warn!("Failed to write Pong: {e}");
                            return ControlLoopExit::StreamClosed;
                        }
                    }
                    Some(control_message::Message::Pong(pong)) => {
                        tracing::trace!("Control: received Pong");
                        if let Some(tx) = pong_tx {
                            let _ = tx.send(pong).await;
                        }
                    }
                    Some(control_message::Message::ControlRequest(req)) => {
                        if let Some(h) = handler {
                            tracing::trace!("Control: handling ControlRequest");
                            let resp = h.handle(req);
                            let msg = ControlMessage {
                                message: Some(control_message::Message::ControlResponse(resp)),
                            };
                            if let Err(e) = write_length_delimited_buf(stream, &msg, &mut write_buf).await {
                                tracing::warn!("Failed to write ControlResponse: {e}");
                                return ControlLoopExit::StreamClosed;
                            }
                        }
                    }
                    Some(control_message::Message::ControlResponse(resp)) => {
                        tracing::trace!("Control: received ControlResponse");
                        if let Some(tx) = control_response_tx {
                            let _ = tx.send(resp).await;
                        }
                    }
                    Some(control_message::Message::Disconnect(dc)) => {
                        tracing::info!("Control: received Disconnect: {}", dc.reason);
                        return ControlLoopExit::Disconnect(dc.reason);
                    }
                    None => {
                        tracing::warn!("Control: received empty ControlMessage");
                    }
                }
            }

            // Write outgoing control messages injected by the caller
            msg = outgoing_rx.recv() => {
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
                #[allow(clippy::cast_possible_truncation)]
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

/// Opens a bidi stream on the transport and runs the control loop (client side).
pub async fn run_control_stream_initiator<T: Transport>(
    transport: &T,
    outgoing_rx: &mut mpsc::Receiver<ControlMessage>,
    handler: Option<&Handler>,
    pong_tx: Option<&mpsc::Sender<wallhack_wire::data::Pong>>,
    control_response_tx: Option<&mpsc::Sender<wallhack_wire::control::ControlResponse>>,
    ping_interval: std::time::Duration,
) -> Result<ControlLoopExit, TransportError> {
    let mut stream = transport.open_bi().await?;
    Ok(run_control_loop(
        &mut stream,
        outgoing_rx,
        handler,
        None, // client doesn't expect Hello
        pong_tx,
        control_response_tx,
        ping_interval,
    )
    .await)
}

/// Accepts the first bidi stream on the transport and runs the control loop (server side).
///
/// The first `ControlMessage` on the accepted stream MUST be a `Hello`.
/// The `hello_tx` oneshot is provided so the caller can receive the hello
/// before deciding whether to spawn data tasks.
pub async fn run_control_stream_acceptor<T: Transport>(
    transport: &T,
    outgoing_rx: &mut mpsc::Receiver<ControlMessage>,
    handler: Option<&Handler>,
    hello_tx: Option<oneshot::Sender<ExitNodeHello>>,
    pong_tx: Option<&mpsc::Sender<wallhack_wire::data::Pong>>,
    ping_interval: std::time::Duration,
) -> Result<ControlLoopExit, TransportError> {
    let Some(mut stream) = transport.accept_bi().await? else {
        return Ok(ControlLoopExit::StreamClosed);
    };
    Ok(run_control_loop(
        &mut stream,
        outgoing_rx,
        handler,
        hello_tx,
        pong_tx,
        None, // server doesn't issue ControlRequests
        ping_interval,
    )
    .await)
}

/// Reads `TunnelMessage`s from a receive stream, dispatching only data
/// messages (instructions and responses) to broadcast channels.
///
/// Unlike `run_incoming_data`, this function does NOT handle Hello, Ping,
/// or Pong — those are handled on the control stream.
pub async fn run_data_in<S: tokio::io::AsyncRead + Unpin>(
    recv: &mut S,
    instructions_tx: &broadcast::Sender<EntryNodeInstruction>,
    responses_tx: &broadcast::Sender<ExitNodeResponse>,
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
                if instructions_tx.send(instr).is_err() {
                    tracing::warn!("No receivers for EntryNodeInstruction");
                }
            }
            Some(tunnel_message::Message::ExitNodeResponse(resp)) => {
                if responses_tx.send(resp).is_err() {
                    tracing::warn!("No receivers for ExitNodeResponse");
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

/// Reads `EntryNodeInstruction`s from a pre-subscribed broadcast receiver and
/// writes them as `TunnelMessage`s on a send stream.
///
/// The caller must subscribe to the broadcast channel **before** calling this
/// function (and before any async work such as `open_uni`) to avoid the race
/// where a message is sent before the subscription is created and silently
/// dropped.
pub async fn run_send_instructions<S: tokio::io::AsyncWrite + Unpin>(
    send: &mut S,
    mut rx: broadcast::Receiver<EntryNodeInstruction>,
) -> Result<(), TransportError> {
    let mut buf = Vec::with_capacity(TUNNEL_MTU);

    loop {
        let instruction = match rx.recv().await {
            Ok(i) => i,
            Err(broadcast::error::RecvError::Closed) => {
                tracing::debug!("Instructions channel closed");
                return Ok(());
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Instructions channel lagged by {n}");
                continue;
            }
        };

        let tunnel_msg = TunnelMessage::from(instruction);
        if let Err(e) = write_length_delimited_buf(send, &tunnel_msg, &mut buf).await {
            tracing::error!("Failed to write instruction: {e}");
            return Err(e);
        }
    }
}

/// Reads `ExitNodeResponse`s from a pre-subscribed broadcast receiver and
/// writes them as `TunnelMessage`s on a send stream.
///
/// The caller must subscribe to the broadcast channel **before** calling this
/// function (and before any async work such as `open_uni`) to avoid the race
/// where a response is sent before the subscription is created and silently
/// dropped. This race is particularly acute for WebSocket/yamux where
/// `open_uni` requires a round-trip through the yamux driver.
pub async fn run_send_responses<S: tokio::io::AsyncWrite + Unpin>(
    send: &mut S,
    mut rx: broadcast::Receiver<ExitNodeResponse>,
) -> Result<(), TransportError> {
    let mut buf = Vec::with_capacity(TUNNEL_MTU);

    loop {
        let response = match rx.recv().await {
            Ok(r) => r,
            Err(broadcast::error::RecvError::Closed) => {
                tracing::debug!("Responses channel closed");
                return Ok(());
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Responses channel lagged by {n}");
                continue;
            }
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

    /// A minimal mock transport for testing bridge functions.
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

        let (instructions_tx, _) = broadcast::channel::<EntryNodeInstruction>(16);
        let (responses_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
        let mut responses_rx = responses_tx.subscribe();

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
                .expect("channel error");
        }

        drop(sender);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), recv_handle).await;
    }

    /// End-to-end: `run_send_responses` → transport → `run_data_in`.
    #[tokio::test]
    async fn test_data_out_to_data_in_roundtrip() {
        let (exit_transport, entry_transport) = MockTransport::pair();

        let (responses_src_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
        let (instructions_dst_tx, _) = broadcast::channel::<EntryNodeInstruction>(16);
        let (responses_dst_tx, _) = broadcast::channel::<ExitNodeResponse>(16);
        let mut responses_dst_rx = responses_dst_tx.subscribe();

        // Subscribe before spawning to avoid the race where messages sent before
        // the task starts are dropped.
        let responses_src_rx = responses_src_tx.subscribe();
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
            responses_src_tx.send(ExitNodeResponse::default()).unwrap();
        }

        for _ in 0..3 {
            tokio::time::timeout(std::time::Duration::from_secs(2), responses_dst_rx.recv())
                .await
                .expect("timed out")
                .expect("channel error");
        }

        drop(responses_src_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), outgoing).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), incoming).await;
    }
}
