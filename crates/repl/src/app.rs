use clap::Parser;
use rustyline::{error::ReadlineError, Editor};

use crate::{
    cli_args::{self, AgentCli},
    error,
    helper::LineHelper,
    info,
    readline::{make_readline, HandlerResult},
    repl_commands::{self, Repl},
    session::{self, Console},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Readline error: {0}")]
    Readline(#[from] ReadlineError),

    #[error("{0}")]
    Server(#[from] wallhack::server::Error),

    #[error("Unspecified error: {0}")]
    Unspecified(#[from] anyhow::Error),
}

impl From<cli_args::ListenArgs> for wallhack::ServerConfig {
    fn from(args: cli_args::ListenArgs) -> Self {
        let listen = args.addr;
        match args.tls {
            Some(tls) => wallhack::ServerConfig {
                listen,
                tls: Some(wallhack::ServerTlsConfig {
                    cert_pem_file: tls.cert_pem_file,
                    key_pem_file: tls.key_pem_file,
                    ca_roots: args.ca_roots,
                }),
            },
            None => wallhack::ServerConfig { listen, tls: None },
        }
    }
}

// Define a struct to hold the REPL state
pub struct HostReplApplication {
    rl: Editor<LineHelper, rustyline::history::FileHistory>,
    app: session::CliSession,
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
    pub fn new(cli: AgentCli) -> anyhow::Result<Self, Error> {
        let mut rl = make_readline()?;
        if rl.load_history("history.txt").is_err() {
            tracing::debug!("No previous history.");
        }

        let name = env!("CARGO_PKG_NAME");
        let prompt = format!("{name} # ");

        let mut app = session::CliSession {
            console: session::Console::default(),
            server: session::NetServer::default(),
        };

        // Print welcome messages
        app.console.stdout.print(format!(
            "{logo}{name} {version}",
            logo = "",
            name = env!("CARGO_PKG_NAME"),
            version = env!("CARGO_PKG_VERSION")
        ));
        app.console.stdout.print("");
        app.console
            .stdout
            .print(format!("Type {} for help.", "help"));
        app.console.stdout.print("");

        // Handle initial 'listen' command if provided via CLI args
        if let cli_args::CliCommands::Listen(args) = cli.command {
            let server_config = wallhack::ServerConfig::from(args);

            app.server.start(server_config)?;
            info!("{}", app.server.get_endpoint_status());
        }

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

                    match shlex::split(&line) {
                        Some(tokens) => match Repl::try_parse_from(tokens) {
                            Ok(console) => {
                                let command_result = self.handle_command(console.command)?;
                                self.rl.append_history("history.txt").unwrap_or_else(|e| {
                                    tracing::warn!("Failed to append history: {e}");
                                });

                                if let HandlerResult::Quit = command_result {
                                    break;
                                }
                            }
                            Err(err) => {
                                if err.kind() == clap::error::ErrorKind::DisplayHelp {
                                    info!("{err}");
                                } else {
                                    error!("Error: {err}");
                                }
                            }
                        },
                        None => {
                            tracing::warn!("Failed to split input line: {}", line);
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
                    // Decide whether to break or continue based on the error
                    // For now, let's break on any readline error other than Interrupt/EOF
                    break;
                }
            }
        }

        info!("bye then");
        Ok(())
    }

    // Helper method to handle individual REPL commands
    fn handle_command(
        &mut self,
        command: repl_commands::ReplCommands,
    ) -> anyhow::Result<HandlerResult> {
        tracing::trace!("Handling command: {:?}", command);
        let result = match command {
            repl_commands::ReplCommands::Quit => HandlerResult::Quit,
            repl_commands::ReplCommands::Listen(args) => {
                let listen_args = args.listen;
                let server_config = wallhack::ServerConfig::from(listen_args);
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
            repl_commands::ReplCommands::Connect(args) => {
                self.app
                    .console
                    .stdout
                    .print(format!("Connecting to {args:?}"));
                // Add connection logic here using self.app
                HandlerResult::Continue
            }
            repl_commands::ReplCommands::Statistics => {
                let _end = self.app.server.get(); // Use self.app.server
                self.app
                    .console
                    .stdout
                    .print("Statistics: Not implemented yet.");
                HandlerResult::Continue
            }
            repl_commands::ReplCommands::Peers => {
                self.app.console.stdout.print("Peers: Not implemented yet.");
                HandlerResult::Continue
            }
            repl_commands::ReplCommands::Clear => {
                Console::clear();
                HandlerResult::Continue
            }
        };
        tracing::trace!("Command result: {:?}", result);
        Ok(result)
    }
}
