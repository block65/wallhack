use super::adapter::{SyscallExitAdapter, TimestampedSession};

use wallhack_exit_adapter::{
    SocketSet,
    adapter::{RuntimeError, SendResponse},
    session::Session,
    session_key::SessionKey,
    sessions::{self, common::RxSession},
};

impl SyscallExitAdapter {
    pub async fn udp_send_impl(
        &self,
        set: SocketSet,
        data: &[u8],
    ) -> Result<SendResponse, RuntimeError> {
        tracing::trace!("Received send data request: {:?}", set);

        let key = SessionKey::Udp(set);

        let is_new =
            if self.sessions.contains_key(&key) {
                false
            } else {
                let local_addr =
                    match set {
                        SocketSet::Ipv4(_) => std::net::SocketAddr::V4(
                            std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0),
                        ),
                        SocketSet::Ipv6(_) => std::net::SocketAddr::V6(
                            std::net::SocketAddrV6::new(std::net::Ipv6Addr::UNSPECIFIED, 0, 0, 0),
                        ),
                    };

                let socket = tokio::net::UdpSocket::bind(local_addr).await?;
                let session = Session::Udp(sessions::udp::UdpSession::new(socket));
                self.sessions
                    .insert(key.clone(), TimestampedSession::new(session));
                true
            };

        let (_, dst_addr) = set.into();

        let response = match self.sessions.get(&key) {
            Some(session) => {
                tracing::trace!("Session: {:?}", session.session);

                if let Session::Udp(session) = &session.value().session {
                    tracing::trace!("Sending data");
                    match session.send(dst_addr, data).await {
                        Ok(sessions::common::SessionStatus::DataIo { size, .. }) => {
                            tracing::trace!("Sent {} bytes to socket", size);
                            SendResponse::Ok {
                                size,
                                set,
                                is_new: Some(is_new),
                            }
                        }
                        Ok(sessions::common::SessionStatus::PeerClosed) => SendResponse::Ok {
                            size: 0,
                            set,
                            is_new: Some(is_new),
                        },
                        Err(e) => return Err(e),
                    }
                } else {
                    return Err(RuntimeError::SessionInvalid(key));
                }
            }
            None => SendResponse::Reset {
                set,
                reason: "session disappeared".to_string(),
            },
        };

        // Touch the session to update last_activity
        if let Some(mut session) = self.sessions.get_mut(&key) {
            session.touch();
        }

        Ok(response)
    }

    pub fn udp_recv_session_impl(
        &self,
        pair: SocketSet,
    ) -> Result<Option<sessions::udp::UdpSession>, RuntimeError> {
        let key = SessionKey::Udp(pair);
        let maybe_session = self.sessions.get(&key);

        match maybe_session {
            Some(session) => {
                if let Session::Udp(session) = &session.value().session {
                    Ok(Some(session.clone()))
                } else {
                    Err(RuntimeError::SessionInvalid(key))
                }
            }
            None => Ok(None),
        }
    }
}
