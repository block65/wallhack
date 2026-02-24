use std::{
	pin::Pin,
	task::{Context, Poll},
};

use criterion::{Criterion, criterion_group, criterion_main};
use futures::{Sink, Stream};
use tokio::{io::AsyncWriteExt, runtime::Runtime};
use tokio_tungstenite::tungstenite::Message;
use wallhack_transport::{
	Transport,
	websocket::{WebSocketByteStream, WebSocketTransport, WebSocketTransportConfig},
};
use yamux::Mode;

/// No-op sink + never-ready stream for measuring the write path in isolation.
struct NullWebSocket;

impl Unpin for NullWebSocket {}

impl Stream for NullWebSocket {
	type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;

	fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		Poll::Pending
	}
}

impl Sink<Message> for NullWebSocket {
	type Error = tokio_tungstenite::tungstenite::Error;

	fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
}

/// Measures the overhead of framing a 64 KiB buffer into a binary WebSocket
/// message and flushing through the adapter — pure in-process cost, no I/O.
fn bench_bytestream_write(c: &mut Criterion) {
	let rt = Runtime::new().unwrap();
	let data = vec![0u8; 64 * 1024];

	c.bench_function("WebSocketByteStream/write_64k", |b| {
		b.iter(|| {
			rt.block_on(async {
				let mut stream = WebSocketByteStream::new(NullWebSocket);
				stream.write_all(&data).await.unwrap();
				stream.flush().await.unwrap();
			});
		});
	});
}

/// Measures the round-trip latency of opening a unidirectional yamux stream:
/// `open_uni` on one side + `accept_uni` on the other, over an in-process
/// duplex pipe. The transport pair is created once and reused across iterations.
fn bench_yamux_stream_open(c: &mut Criterion) {
	let rt = Runtime::new().unwrap();

	let (client, server) = rt.block_on(async {
		let (s1, s2) = tokio::io::duplex(64 * 1024);
		let (client, client_driver) =
			WebSocketTransport::new(s1, Mode::Client, None, WebSocketTransportConfig::default());
		let (server, server_driver) =
			WebSocketTransport::new(s2, Mode::Server, None, WebSocketTransportConfig::default());
		tokio::spawn(async move { client_driver.await.ok() });
		tokio::spawn(async move { server_driver.await.ok() });
		(client, server)
	});

	c.bench_function("yamux/stream_open_round_trip", |b| {
		b.iter(|| {
			rt.block_on(async {
				let _send = client.open_uni().await.unwrap();
				let _recv = server.accept_uni().await.unwrap();
			});
		});
	});
}

criterion_group!(benches, bench_bytestream_write, bench_yamux_stream_open);
criterion_main!(benches);
