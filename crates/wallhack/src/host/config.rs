use crate::{ClientConfig, ServerConfig};

pub enum RoleConfig {
	Server(ServerConfig),
	Client(ClientConfig),
}

pub struct HostConfig {
	pub tun: Option<String>,

	pub role: RoleConfig,
}
