//! Object-safe transport erasure.
//!
//! The [`Transport`] trait uses RPITIT and associated types, making it
//! non-object-safe. This module provides [`ErasedTransport`], a parallel
//! trait with boxed futures and erased stream types, plus a blanket impl
//! over all `T: Transport`. Code that was previously monomorphized over a
//! concrete transport can instead accept `Arc<dyn ErasedTransport>`,
//! collapsing multiple instantiations into one.

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{BiStream, Transport, TransportError};

// ---------------------------------------------------------------------------
// Type aliases for boxed return types (keeps clippy::type_complexity happy)
// ---------------------------------------------------------------------------

type BoxSendStream = Box<dyn AsyncWrite + Send + Unpin>;
type BoxRecvStream = Box<dyn AsyncRead + Send + Unpin>;
type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// BoxBiStream — object-safe wrapper for BiStream
// ---------------------------------------------------------------------------

/// Object-safe helper trait (private). Mirrors [`BiStream`] but with a boxed
/// future for `finish()`.
///
/// The `Unpin` bound is inherited from `BiStream`'s supertrait chain
/// (`AsyncRead + AsyncWrite + Unpin`), so all `T: BiStream` satisfy it.
trait DynBiStream: AsyncRead + AsyncWrite + Send + Unpin {
    fn finish_dyn(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;
}

impl<T: BiStream + Send> DynBiStream for T {
    fn finish_dyn(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(BiStream::finish(self))
    }
}

/// A type-erased bidirectional stream.
///
/// Wraps any concrete `BiStream` behind a `Box<dyn …>`, forwarding
/// `AsyncRead`, `AsyncWrite`, and `finish()` through vtable dispatch.
pub struct BoxBiStream(Box<dyn DynBiStream>);

impl BoxBiStream {
    /// Wrap a concrete `BiStream` into a `BoxBiStream`.
    pub fn new<S: BiStream + Send + 'static>(stream: S) -> Self {
        Self(Box::new(stream))
    }
}

impl AsyncRead for BoxBiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxBiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

impl BiStream for BoxBiStream {
    async fn finish(&mut self) -> Result<(), TransportError> {
        self.0.finish_dyn().await
    }
}

// ---------------------------------------------------------------------------
// ErasedTransport — object-safe transport trait
// ---------------------------------------------------------------------------

/// Object-safe counterpart of [`Transport`].
///
/// All futures are boxed and stream types are erased to trait objects.
/// A blanket impl covers every `T: Transport + Send + Sync` so call sites
/// can coerce `Arc<T>` to `Arc<dyn ErasedTransport>` at zero effort.
pub trait ErasedTransport: Send + Sync {
    /// Open a unidirectional send stream.
    fn open_uni_erased(&self) -> BoxFut<'_, Result<BoxSendStream, TransportError>>;

    /// Open a bidirectional stream.
    fn open_bi_erased(&self) -> BoxFut<'_, Result<BoxBiStream, TransportError>>;

    /// Accept an incoming unidirectional receive stream.
    fn accept_uni_erased(&self) -> BoxFut<'_, Result<Option<BoxRecvStream>, TransportError>>;

    /// Accept an incoming bidirectional stream.
    fn accept_bi_erased(&self) -> BoxFut<'_, Result<Option<BoxBiStream>, TransportError>>;

    /// Close the transport.
    fn close_erased(&self) -> BoxFut<'_, Result<(), TransportError>>;

    /// Remote peer address, if known.
    fn remote_addr(&self) -> Option<SocketAddr>;
}

impl<T> ErasedTransport for T
where
    T: Transport + Send + Sync,
    T::SendStream: 'static,
    T::RecvStream: 'static,
    T::BiStream: Send + 'static,
{
    fn open_uni_erased(&self) -> BoxFut<'_, Result<BoxSendStream, TransportError>> {
        Box::pin(async {
            let s = Transport::open_uni(self).await?;
            Ok(Box::new(s) as BoxSendStream)
        })
    }

    fn open_bi_erased(&self) -> BoxFut<'_, Result<BoxBiStream, TransportError>> {
        Box::pin(async {
            let s = Transport::open_bi(self).await?;
            Ok(BoxBiStream::new(s))
        })
    }

    fn accept_uni_erased(&self) -> BoxFut<'_, Result<Option<BoxRecvStream>, TransportError>> {
        Box::pin(async {
            let opt = Transport::accept_uni(self).await?;
            Ok(opt.map(|s| Box::new(s) as BoxRecvStream))
        })
    }

    fn accept_bi_erased(&self) -> BoxFut<'_, Result<Option<BoxBiStream>, TransportError>> {
        Box::pin(async {
            let opt = Transport::accept_bi(self).await?;
            Ok(opt.map(BoxBiStream::new))
        })
    }

    fn close_erased(&self) -> BoxFut<'_, Result<(), TransportError>> {
        Box::pin(Transport::close(self))
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        Transport::remote_addr(self)
    }
}

// Convenience: let Arc<dyn ErasedTransport> itself be used as an ErasedTransport.
impl ErasedTransport for Arc<dyn ErasedTransport> {
    fn open_uni_erased(&self) -> BoxFut<'_, Result<BoxSendStream, TransportError>> {
        (**self).open_uni_erased()
    }

    fn open_bi_erased(&self) -> BoxFut<'_, Result<BoxBiStream, TransportError>> {
        (**self).open_bi_erased()
    }

    fn accept_uni_erased(&self) -> BoxFut<'_, Result<Option<BoxRecvStream>, TransportError>> {
        (**self).accept_uni_erased()
    }

    fn accept_bi_erased(&self) -> BoxFut<'_, Result<Option<BoxBiStream>, TransportError>> {
        (**self).accept_bi_erased()
    }

    fn close_erased(&self) -> BoxFut<'_, Result<(), TransportError>> {
        (**self).close_erased()
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        (**self).remote_addr()
    }
}
