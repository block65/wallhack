//! Lightweight tracing subscriber with substring-based filtering.
//!
//! Replaces `tracing-subscriber` + `env-filter` to avoid pulling in the regex
//! stack (~400KB of `.text`). Filters are comma-separated substrings matched
//! against the tracing event's module path.

use tracing::{Event, Level, Metadata, Subscriber, level_filters::LevelFilter};

/// A minimal [`Subscriber`] that filters by level and optional module substrings.
pub struct SimpleSubscriber {
	max_level: LevelFilter,
	filters: Vec<String>,
}

impl SimpleSubscriber {
	/// Create a new subscriber.
	///
	/// - `max_level`: maximum tracing level to emit.
	/// - `filters`: comma-separated substring list (empty string = no filtering).
	#[must_use]
	pub fn new(max_level: LevelFilter, filter_str: &str) -> Self {
		let filters: Vec<String> = filter_str
			.split(',')
			.map(str::trim)
			.filter(|s| !s.is_empty())
			.map(String::from)
			.collect();

		Self { max_level, filters }
	}
}

impl Subscriber for SimpleSubscriber {
	fn enabled(&self, metadata: &Metadata<'_>) -> bool {
		if *metadata.level() > self.max_level {
			return false;
		}
		if self.filters.is_empty() {
			return true;
		}
		let module = metadata.module_path().unwrap_or("");
		self.filters.iter().any(|f| module.contains(f.as_str()))
	}

	fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
		tracing::span::Id::from_u64(1)
	}

	fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

	fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

	fn event(&self, event: &Event<'_>) {
		let meta = event.metadata();
		let level = *meta.level();
		let module = meta.module_path().unwrap_or("<unknown>");

		// Extract the message field from the event.
		let mut visitor = MessageVisitor(String::new());
		event.record(&mut visitor);

		let tag = match level {
			Level::ERROR => "ERROR",
			Level::WARN => " WARN",
			Level::INFO => " INFO",
			Level::DEBUG => "DEBUG",
			Level::TRACE => "TRACE",
		};
		crate::repl_common::emit_log(format!("[{tag}] {module}: {}", visitor.0));
	}

	fn enter(&self, _span: &tracing::span::Id) {}

	fn exit(&self, _span: &tracing::span::Id) {}
}

/// Visitor that captures the `message` field from a tracing event.
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
	fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
		if field.name() == "message" {
			self.0 = format!("{value:?}");
		} else if self.0.is_empty() {
			self.0 = format!("{} = {value:?}", field.name());
		} else {
			self.0 = format!("{}, {} = {value:?}", self.0, field.name());
		}
	}

	fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
		if field.name() == "message" {
			self.0 = value.to_string();
		} else if self.0.is_empty() {
			self.0 = format!("{} = {value}", field.name());
		} else {
			self.0 = format!("{}, {} = {value}", self.0, field.name());
		}
	}
}
