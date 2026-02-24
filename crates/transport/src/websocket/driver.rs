//! Yamux connection driver for WebSocket transport.

use std::{
	collections::VecDeque,
	future::Future,
	pin::Pin,
	task::{Context, Poll},
	time::Duration,
};

use futures::{AsyncReadExt as FuturesAsyncReadExt, StreamExt as _, stream::FuturesUnordered};
use tokio::{
	io::{AsyncRead, AsyncWrite},
	sync::{mpsc, oneshot},
};
use tokio_util::compat::TokioAsyncReadCompatExt;
use tracing::{debug, error, warn};
use yamux::{Connection, ConnectionError, Mode, Stream as YamuxStream};

use crate::TransportError;

/// Stream type prefix for data/unidirectional streams.
pub(super) const STREAM_TYPE_DATA: u8 = 0x00;

/// Stream type prefix for control/bidirectional streams.
pub(super) const STREAM_TYPE_CONTROL: u8 = 0x01;

/// Maximum number of stream-open requests that can be queued while yamux is
/// stalled (e.g. flow-control window exhausted).
const MAX_PENDING_STREAM_OPENS: usize = 256;

/// Maximum number of inbound streams undergoing prefix classification at once.
/// Excess streams are closed immediately.
const MAX_CONCURRENT_CLASSIFICATIONS: usize = 128;

/// Trait alias for types that implement both tokio [`AsyncRead`] and
/// [`AsyncWrite`].
pub trait TokioAsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> TokioAsyncReadWrite for T {}

/// Commands sent to the driver task.
pub(super) enum Command {
	OpenUni(oneshot::Sender<Result<YamuxStream, TransportError>>),
	OpenBi(oneshot::Sender<Result<YamuxStream, TransportError>>),
	Close,
}

type PrefixReadFut = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The driver task that manages the yamux connection and dispatches inbound
/// streams.
///
/// Implements [`Future`] and must be spawned to drive the yamux connection.
pub struct Driver {
	pub(super) connection: Connection<tokio_util::compat::Compat<Box<dyn TokioAsyncReadWrite>>>,
	pub(super) cmd_rx: mpsc::Receiver<Command>,
	pub(super) incoming_uni_tx: mpsc::Sender<YamuxStream>,
	pub(super) incoming_bi_tx: mpsc::Sender<YamuxStream>,
	pub(super) pending_open_uni: VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
	pub(super) pending_open_bi: VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
	pub(super) pending_prefix_reads: FuturesUnordered<PrefixReadFut>,
	pub(super) prefix_read_timeout: Duration,
	pub(super) shutdown: bool,
}

impl Driver {
	fn poll_commands(&mut self, cx: &mut Context<'_>) -> bool {
		let mut progress = false;
		while let Poll::Ready(Some(cmd)) = self.cmd_rx.poll_recv(cx) {
			progress = true;
			match cmd {
				Command::OpenUni(tx) => {
					if self.pending_open_uni.len() + self.pending_open_bi.len()
						>= MAX_PENDING_STREAM_OPENS
					{
						warn!("pending stream-open queue full; rejecting open_uni request");
						let _ = tx.send(Err(TransportError::Overloaded));
					} else {
						self.pending_open_uni.push_back(tx);
					}
				}
				Command::OpenBi(tx) => {
					if self.pending_open_uni.len() + self.pending_open_bi.len()
						>= MAX_PENDING_STREAM_OPENS
					{
						warn!("pending stream-open queue full; rejecting open_bi request");
						let _ = tx.send(Err(TransportError::Overloaded));
					} else {
						self.pending_open_bi.push_back(tx);
					}
				}
				Command::Close => {
					debug!("WebSocket transport driver received close command");
					self.shutdown = true;
					break;
				}
			}
		}
		progress
	}

	fn poll_pending_opens(&mut self, cx: &mut Context<'_>) -> bool {
		let mut progress = false;

		if let Some(tx) = self.pending_open_uni.pop_front() {
			match self.connection.poll_new_outbound(cx) {
				Poll::Ready(Ok(stream)) => {
					progress = true;
					debug!("opened outbound unidirectional stream");
					let _ = tx.send(Ok(stream));
				}
				Poll::Ready(Err(e)) => {
					progress = true;
					error!(error = %e, "failed to open outbound unidirectional stream");
					let _ = tx.send(Err(TransportError::connection_closed(e.to_string())));
				}
				Poll::Pending => self.pending_open_uni.push_front(tx),
			}
		}

		if let Some(tx) = self.pending_open_bi.pop_front() {
			match self.connection.poll_new_outbound(cx) {
				Poll::Ready(Ok(stream)) => {
					progress = true;
					debug!("opened outbound bidirectional stream");
					let _ = tx.send(Ok(stream));
				}
				Poll::Ready(Err(e)) => {
					progress = true;
					error!(error = %e, "failed to open outbound bidirectional stream");
					let _ = tx.send(Err(TransportError::connection_closed(e.to_string())));
				}
				Poll::Pending => self.pending_open_bi.push_front(tx),
			}
		}

		progress
	}

	fn poll_inbound(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, ConnectionError>> {
		match self.connection.poll_next_inbound(cx) {
			Poll::Ready(Some(Ok(mut stream))) => {
				if self.pending_prefix_reads.len() >= MAX_CONCURRENT_CLASSIFICATIONS {
					warn!("stream classification limit reached; dropping inbound stream");
					drop(stream);
					return Poll::Ready(Ok(true));
				}

				let uni_tx = self.incoming_uni_tx.clone();
				let bi_tx = self.incoming_bi_tx.clone();
				let timeout = self.prefix_read_timeout;

				let fut: PrefixReadFut = Box::pin(async move {
					let mut prefix = [0u8; 1];
					let read_result =
						tokio::time::timeout(timeout, stream.read_exact(&mut prefix)).await;

					match read_result {
						Ok(Ok(())) => match prefix[0] {
							STREAM_TYPE_DATA => {
								debug!("accepted inbound unidirectional stream");
								let _ = uni_tx.send(stream).await;
							}
							STREAM_TYPE_CONTROL => {
								debug!("accepted inbound bidirectional stream");
								let _ = bi_tx.send(stream).await;
							}
							unknown => {
								warn!(
									prefix = unknown,
									"unknown stream type prefix; dropping stream"
								);
							}
						},
						Ok(Err(e)) => {
							error!(error = %e, "error reading stream type prefix; dropping stream");
						}
						Err(_elapsed) => {
							warn!("timed out reading stream type prefix; dropping stream");
						}
					}
				});

				self.pending_prefix_reads.push(fut);

				Poll::Ready(Ok(true))
			}
			Poll::Ready(Some(Err(e))) => {
				error!(error = %e, "yamux connection error");
				Poll::Ready(Err(e))
			}
			Poll::Ready(None) => Poll::Ready(Ok(false)),
			Poll::Pending => Poll::Pending,
		}
	}

	fn poll_prefix_reads(&mut self, cx: &mut Context<'_>) -> bool {
		let mut progress = false;
		while let Poll::Ready(Some(())) = self.pending_prefix_reads.poll_next_unpin(cx) {
			progress = true;
		}
		progress
	}
}

impl Future for Driver {
	type Output = Result<(), ConnectionError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();

		loop {
			let mut progress = false;

			if !this.shutdown {
				progress |= this.poll_commands(cx);
			}

			progress |= this.poll_pending_opens(cx);

			if this.shutdown {
				match this.connection.poll_close(cx) {
					Poll::Ready(Ok(())) => return Poll::Ready(Ok(())),
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
					Poll::Pending => {}
				}
			} else {
				match this.poll_inbound(cx) {
					Poll::Ready(Ok(true)) => progress = true,
					Poll::Ready(Ok(false)) => return Poll::Ready(Ok(())),
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
					Poll::Pending => {}
				}
			}

			progress |= this.poll_prefix_reads(cx);

			if !progress {
				return Poll::Pending;
			}
		}
	}
}

/// Creates a boxed, compat-wrapped connection over a `TokioAsyncReadWrite`
/// stream.
pub(super) fn make_connection<S>(
	stream: S,
	mode: Mode,
	yamux_config: yamux::Config,
) -> Connection<tokio_util::compat::Compat<Box<dyn TokioAsyncReadWrite>>>
where
	S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
	let boxed: Box<dyn TokioAsyncReadWrite> = Box::new(stream);
	Connection::new(boxed.compat(), yamux_config, mode)
}
