use std::{
	fmt::{Display, Formatter},
	net::SocketAddr,
};

use crate::styles::OutputStyles;

#[derive(Default)]
pub struct Printer {
	styles: OutputStyles,
}

impl Printer {
	pub fn print(&self, text: impl Into<String>) {
		let text = text.into();
		println!(
			"{style}{}{style:#}",
			text,
			style = self.styles.get_literal()
		);
	}
	// pub fn print_err(&self, text: impl Into<String>) {
	// 	let text = text.into();
	// 	eprintln!("{style}{}{style:#}", text, style = self.styles.get_error());
	// }
}

#[derive(Default)]
pub struct Console {
	pub stdout: Printer,
}

impl Console {
	pub fn clear() {
		print!("\x1b[H\x1b[J");
	}
}

#[derive(Default, Debug)]
pub struct NetServer {
	// _num_ctrlc: bool,
	endpoint: Option<quinn::Endpoint>,
}

trait _StatsForNerds {
	type StatsType;

	fn nerds_stats(&self) -> Self::StatsType;

	fn nerds_status(&self) -> String;
}

impl _StatsForNerds for NetServer {
	type StatsType = String;

	fn nerds_stats(&self) -> String {
		"Stats: Not implemented yet.".to_string()
	}
	fn nerds_status(&self) -> String {
		if let Some(e) = self.endpoint.as_ref() {
			format!("Server is already listening on {:?}", e.local_addr())
		} else {
			"Server is not listening".to_string()
		}
	}
}

impl NetServer {
	pub fn start(&mut self, config: ServerConfig) -> Result<(), wallhack::server::Error> {
		tracing::trace!("config: {:?}.", config);
		self.endpoint = Some(server::create(config)?);
		Ok(())
	}

	pub async fn _stop(&mut self) -> Result<(), wallhack::server::Error> {
		if let Some(endp) = self.endpoint.take() {
			endp.close(0u32.into(), b"stop request");
			tracing::trace!("Server stopping...  Wait for idle.");
			endp.wait_idle().await;
			tracing::trace!("Server stopped.");
			self.endpoint = None;
		}
		Ok(())
	}

	#[must_use]
	pub fn get(&self) -> Option<&quinn::Endpoint> {
		self.endpoint.as_ref()
	}

	#[must_use]
	pub fn has(&self) -> bool {
		self.endpoint.is_some()
	}

	#[must_use]
	pub fn get_endpoint_status(&self) -> Status {
		if let Some(e) = self.endpoint.as_ref() {
			match e.local_addr() {
				Ok(addr) => Status {
					socket_addr: Some(DisplayableSocketAddr(addr)),
				},
				Err(_) => Status { socket_addr: None },
			}
		} else {
			Status { socket_addr: None }
		}
	}
}

pub struct Status {
	socket_addr: Option<DisplayableSocketAddr>,
}

impl Display for Status {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		if let Some(addr) = &self.socket_addr {
			write!(f, "Listening on {addr}",)
		} else {
			write!(f, "Not listening")
		}
	}
}

pub struct DisplayableSocketAddr(SocketAddr);

impl Display for DisplayableSocketAddr {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

pub struct CliSession {
	pub console: Console,
	pub server: NetServer,
}

impl _StatsForNerds for CliSession {
	type StatsType = String;

	fn nerds_stats(&self) -> Self::StatsType {
		self.server.nerds_stats()
	}

	fn nerds_status(&self) -> String {
		self.server.nerds_status()
	}
}

#[cfg(test)]
mod tests {

	use super::*;

	#[test]
	fn test_cli_session() {
		let cli_session = CliSession {
			console: Console::default(),
			server: NetServer::default(),
		};
		assert_eq!(cli_session.nerds_status(), "Server is not listening");
		// Note: This test only ensures the method runs without panicking.
	}

	#[test]
	fn test_printer_print() {
		let printer = Printer::default();
		printer.print("Hello, world!");
		// Note: This test only ensures the method runs without panicking.
	}

	/* 	#[test]
	fn test_printer_print_err() {
		let printer = Printer;
		printer.print_err("Test only! Please ignore");
		// Note: This test only ensures the method runs without panicking.
	} */

	#[test]
	fn test_console_clear() {
		// let console = Console::default();
		Console::clear();
		// Note: This test only ensures the method runs without panicking.
	}

	#[test]
	fn test_initial_state() {
		let state = NetServer::default();
		// assert!(state.workspace.is_none());
		// assert!(!state.num_ctrlc);
		assert!(state.endpoint.is_none());
	}
}
