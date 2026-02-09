use anstyle::{AnsiColor, Style};

pub struct OutputStyles {
	_header: Style,
	error: Style,
	_usage: Style,
	literal: Style,
	_placeholder: Style,
	valid: Style,
	invalid: Style,
}

impl Default for OutputStyles {
	fn default() -> Self {
		#[cfg(feature = "color")]
		{
			Self {
				_header: AnsiColor::Yellow.on_default(),
				error: AnsiColor::Red.on_default(),
				_usage: AnsiColor::Green.on_default(),
				literal: AnsiColor::Green.on_default(),
				_placeholder: AnsiColor::Green.on_default(),
				valid: AnsiColor::Green.on_default(),
				invalid: AnsiColor::Yellow.on_default(),
			}
		}
		#[cfg(not(feature = "color"))]
		{
			Self {
				_header: AnsiColor::White.on_default(),
				error: AnsiColor::White.on_default(),
				_usage: AnsiColor::White.on_default(),
				literal: AnsiColor::White.on_default(),
				_placeholder: AnsiColor::White.on_default(),
				valid: AnsiColor::White.on_default(),
				_invalid: AnsiColor::White.on_default(),
			}
		}
	}
}

impl OutputStyles {
	#[must_use]
	pub const fn get_literal(&self) -> &Style {
		&self.literal
	}

	#[must_use]
	pub const fn get_error(&self) -> &Style {
		&self.error
	}

	#[must_use]
	pub const fn get_warning(&self) -> &Style {
		&self.invalid
	}

	#[must_use]
	pub const fn get_valid(&self) -> &Style {
		&self.valid
	}
}
