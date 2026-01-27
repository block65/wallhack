use rustyline::{Editor, error::ReadlineError};

use crate::{
	error,
	helper::LineHelper,
	info,
	readline::{HandlerResult, make_readline},
	repl_commands::{Repl, ReplCommands},
	session::{CliSession, Console, NetServer},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("Readline error: {0}")]
	Readline(#[from] ReadlineError),

	#[error("{0}")]
	Server(#[from] wallhack::server::Error),

	#[error("Unspecified error: {0}")]
	Unspecified(#[from] anyhow::Error),

	#[error("Config error: {0}")]
	Config(String),
}

// Define a struct to hold the REPL state
pub struct HostReplApplication {
	rl: Editor<LineHelper, rustyline::history::FileHistory>,
	app: CliSession,
	prompt: String,
}

impl HostReplApplication {
	/// Creates a new instance of `HostReplApplication`.
	///
	/// # Errors
	///
	/// This function will return an error if:
	/// - The readline history cannot be loaded.
	/// - The server fails to start due to invalid configuration.
	pub fn new(_cli: crate::AgentCli) -> anyhow::Result<Self, Error> {
		let mut rl = make_readline().map_err(Error::Unspecified)?;
		if rl.load_history("history.txt").is_err() {
			tracing::debug!("No previous history.");
		}

		let name = env!("CARGO_PKG_NAME");
		let prompt = format!("{name} # ");

		let app = CliSession {
			console: Console::default(),
			server: NetServer::default(),
		};

		// Print welcome messages
		app.console.stdout.print(format!(
			"{name} {version}",
			name = env!("CARGO_PKG_NAME"),
			version = env!("CARGO_PKG_VERSION")
		));
		app.console.stdout.print("");
		app.console
			.stdout
			.print(format!("Type {} for help.", "help"));
		app.console.stdout.print("");

		// Note: Initial command from _cli is ignored for now to simplify
		// but could be implemented by converting AgentCli to ReplCommands

		Ok(Self { rl, app, prompt })
	}

	/// Runs the REPL loop.
	///
	/// # Errors
	///
	/// This function will return an error if:
	/// - A readline error occurs while reading input.
	/// - A command execution fails due to an internal error.
	pub fn run(&mut self) -> anyhow::Result<(), Error> {
		loop {
			tracing::trace!("readline loop");
			let readline = self.rl.readline(&self.prompt);
			tracing::trace!("readline result: {:?}", readline);

			match readline {
				Ok(line) => {
					if line.is_empty() {
						continue;
					}

					self.rl.add_history_entry(line.as_str())?;

					match Repl::parse_line(&line) {
						Ok(repl) => {
							let command_result = self.handle_command(repl.command)?;
							self.rl.append_history("history.txt").unwrap_or_else(|e| {
								tracing::warn!("Failed to append history: {e}");
							});

							if let HandlerResult::Quit = command_result {
								break;
							}
						}
						Err(err) => {
							error!("Error: {err}");
						}
					}
				}
				Err(ReadlineError::Interrupted) => {
					tracing::trace!("CTRL-C");
					break;
				}
				Err(ReadlineError::Eof) => {
					tracing::trace!("CTRL-D");
					break;
				}
				Err(err) => {
					error!("Readline Error: {err:?}");
					break;
				}
			}
		}

		info!("bye then");
		Ok(())
	}

	// Helper method to handle individual REPL commands
	fn handle_command(&mut self, command: ReplCommands) -> anyhow::Result<HandlerResult, Error> {
		tracing::trace!("Handling command: {:?}", command);
		let result = match command {
			ReplCommands::Quit(_) => HandlerResult::Quit,
			ReplCommands::Listen(args) => {
				let addr = args
					.addr
					.parse::<std::net::SocketAddr>()
					.map_err(|e| Error::Config(format!("Invalid address: {e}")))?;

				let server_config = wallhack::server::config::ServerConfig {
					listen: addr,
					tls: match (args.cert, args.key) {
						(Some(cert), Some(key)) => Some(wallhack::server::config::TlsConfig {
							cert_pem_file: cert,
							key_pem_file: key,
							ca_roots: args.ca,
						}),
						_ => None,
					},
				};

				if !self.app.server.has() {
					self.app.server.start(server_config)?;
				}
				let status = self.app.server.get_endpoint_status();
				self.app
					.console
					.stdout
					.print(format!("Server status: {status}"));
				HandlerResult::Continue
			}
			ReplCommands::Connect(args) => {
				self.app
					.console
					.stdout
					.print(format!("Connecting to {args:?}"));
				// Connect logic will be added here
				HandlerResult::Continue
			}
			ReplCommands::Stats(_) => {
				self.app
					.console
					.stdout
					.print("Statistics: Not implemented yet.");
				HandlerResult::Continue
			}
			ReplCommands::Peers(_) => {
				self.app.console.stdout.print("Peers: Not implemented yet.");
				HandlerResult::Continue
			}
			ReplCommands::Clear(_) => {
				Console::clear();
				HandlerResult::Continue
			}
		};
		tracing::trace!("Command result: {:?}", result);
		Ok(result)
	}
}
