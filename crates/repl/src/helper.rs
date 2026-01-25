use rustyline::{Completer, Helper, Highlighter, Hinter, Validator};

use crate::repl_commands::Repl;

use super::completer::ClapCompleter;

#[derive(Helper, Completer, Validator, Highlighter, Hinter)]
pub struct LineHelper {
	#[rustyline(Completer)]
	completer: ClapCompleter,
}

impl LineHelper {
	pub fn new() -> Self {
		let commands = Repl::command_names().into_iter().map(String::from).collect();

		Self {
			completer: ClapCompleter::new(commands),
		}
	}
}

impl Default for LineHelper {
	fn default() -> Self {
		Self::new()
	}
}
