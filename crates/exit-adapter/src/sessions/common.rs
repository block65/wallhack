use std::net::SocketAddr;

use crate::adapter::RuntimeError;

#[derive(Debug)]
pub enum SessionStatus {
    DataIo { size: usize },
    PeerClosed,
}
pub trait RxSession {
    // fn status(&self) -> SessionStatus;

    fn send(
        &self,
        dest: SocketAddr,
        buf: &[u8],
    ) -> impl std::future::Future<Output = Result<SessionStatus, RuntimeError>> + Send;

    fn recv(
        &self,
        buf: &mut [u8],
    ) -> impl std::future::Future<Output = Result<SessionStatus, RuntimeError>> + Send;

    // fn close(&self) -> Result<(), std::io::Error>;
}
