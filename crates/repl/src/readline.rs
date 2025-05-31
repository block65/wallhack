use anyhow::Result;
use rustyline::{CompletionType, EditMode, Editor};

use crate::helper::LineHelper;

pub fn make_readline() -> Result<Editor<LineHelper, rustyline::history::FileHistory>> {
	let config = rustyline::Config::builder()
		.history_ignore_space(true)
		.auto_add_history(true)
		.completion_type(CompletionType::List)
		.edit_mode(EditMode::Emacs)
		.build();

	let mut rl = Editor::with_config(config)?;

	let h = LineHelper::new();
	rl.set_helper(Some(h));

	Ok(rl)
}

#[derive(Debug)]
pub enum HandlerResult {
	Continue,

	Quit,
}
