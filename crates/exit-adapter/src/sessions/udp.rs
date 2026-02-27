use std::{net::SocketAddr, sync::Arc};

use crate::adapter::RuntimeError;

use super::common::{RxSession, SessionStatus};

#[derive(Debug, Clone)]
pub struct UdpSession {
    socket: Arc<tokio::net::UdpSocket>,
}

impl UdpSession {
    pub fn new(socket: tokio::net::UdpSocket) -> Self {
        Self {
            socket: Arc::new(socket),
        }
    }
}

impl RxSession for UdpSession {
    async fn send(&self, dst_addr: SocketAddr, buf: &[u8]) -> Result<SessionStatus, RuntimeError> {
        loop {
            self.socket.writable().await?;
            match self.socket.try_send_to(buf, dst_addr) {
                Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(io_err) => {
                    return match io_err.kind() {
                        std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
                        _ => Err(RuntimeError::from(io_err)),
                    };
                }
            }
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
        loop {
            self.socket.readable().await?;
            match self.socket.try_recv_from(buf) {
                Ok((n, _)) => return Ok(SessionStatus::DataIo { size: n }),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(io_err) => {
                    return match io_err.kind() {
                        std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
                        _ => Err(RuntimeError::from(io_err)),
                    };
                }
            }
        }
    }
}
