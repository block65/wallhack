use protobuf::{SocketSet, v2};

pub trait HostAdapter: Send + Sync {
	type Error: std::error::Error + Send + Sync + 'static;

	fn next_message(
		&self,
		buf: &mut [u8],
	) -> impl std::future::Future<Output = Result<Vec<v2::tunnel_message::Message>, Self::Error>> + Send;

	fn handle_response(
		&self,
		set: SocketSet,
		response: v2::agent_response::Response,
	) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}
