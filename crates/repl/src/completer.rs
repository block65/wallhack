use rustyline::{Context, completion::Completer};

#[derive(Hash, Debug, PartialEq, Eq)]
pub struct ClapCompleter {
	commands: Vec<String>,
}

impl ClapCompleter {
	pub fn new(commands: Vec<String>) -> Self {
		Self { commands }
	}

	// pub fn set_commands(&mut self, commands: Vec<String>) {
	// 	self.commands = commands;
	// }
}

impl Completer for ClapCompleter {
	type Candidate = String;

	fn complete(
		&self,
		line: &str,
		pos: usize,
		_: &Context<'_>,
	) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
		let (start, end) = line[..pos].rfind(' ').map_or((0, pos), |i| (i + 1, pos));

		let prefix = &line[start..end];
		let candidates = self
			.commands
			.iter()
			.filter(|cmd| cmd.starts_with(prefix))
			.map(std::string::ToString::to_string)
			.collect();

		Ok((start, candidates))
	}
}
