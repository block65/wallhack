//! Lightweight tracing subscriber with substring-based filtering and dedup.
//!
//! Replaces `tracing-subscriber` + `env-filter` to avoid pulling in the regex
//! stack (~400KB of `.text`). Filters are comma-separated substrings matched
//! against the tracing event's module path.
//!
//! Consecutive identical log lines are suppressed and summarised as
//! `"↑ repeated N×"` when the streak breaks.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, RwLock},
};

use tracing::{Event, Level, Metadata, Subscriber, level_filters::LevelFilter};
use wallhack_core::control::log_buffer::LogBuffer;

pub type LogWriter = Arc<RwLock<Box<dyn Fn(&str, &str) + Send + Sync>>>;

struct DedupeState {
    last_hash: u64,
    last_tag: &'static str,
    count: usize,
}

/// A minimal [`Subscriber`] that filters by level and optional module substrings.
pub struct SimpleSubscriber {
    max_level: LevelFilter,
    filters: Vec<String>,
    writer: LogWriter,
    log_buffer: Option<LogBuffer>,
    dedup: Mutex<DedupeState>,
}

impl SimpleSubscriber {
    /// Create a new subscriber.
    ///
    /// - `max_level`: maximum tracing level to emit.
    /// - `filter_str`: comma-separated substring list (empty string = no filtering).
    #[must_use]
    pub fn new(max_level: LevelFilter, filter_str: &str) -> Self {
        let filters: Vec<String> = filter_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        Self {
            max_level,
            filters,
            writer: Arc::new(RwLock::new(Box::new(|tag, msg| eprintln!("{tag}: {msg}")))),
            log_buffer: None,
            dedup: Mutex::new(DedupeState {
                last_hash: 0,
                last_tag: "info",
                count: 0,
            }),
        }
    }

    /// Returns a handle to the writer so the caller can swap the destination.
    #[must_use]
    pub fn writer(&self) -> LogWriter {
        Arc::clone(&self.writer)
    }

    /// Attach a [`LogBuffer`] so every emitted line is also stored in memory.
    pub fn set_log_buffer(&mut self, buffer: LogBuffer) {
        self.log_buffer = Some(buffer);
    }
}

impl From<&crate::daemon_cli::WallhackCli> for SimpleSubscriber {
    fn from(cli: &crate::daemon_cli::WallhackCli) -> Self {
        if cli.trace || cli.trace_filter.is_some() {
            Self::new(
                LevelFilter::TRACE,
                cli.trace_filter.as_deref().unwrap_or(""),
            )
        } else if cli.debug || cli.debug_filter.is_some() {
            Self::new(
                LevelFilter::DEBUG,
                cli.debug_filter.as_deref().unwrap_or(""),
            )
        } else {
            Self::new(LevelFilter::INFO, "")
        }
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
        let level = *event.metadata().level();

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let tag = match level {
            Level::ERROR => "error",
            Level::WARN => "warn",
            Level::INFO => "info",
            Level::DEBUG => "debug",
            Level::TRACE => "trace",
        };

        // Hash message + level + target to detect consecutive duplicates.
        let mut hasher = DefaultHasher::new();
        visitor.0.hash(&mut hasher);
        level.hash(&mut hasher);
        event.metadata().target().hash(&mut hasher);
        let hash = hasher.finish();

        let Ok(mut dedup) = self.dedup.lock() else {
            return;
        };

        if hash == dedup.last_hash {
            dedup.count += 1;
            return;
        }

        // Streak broken — flush repeat count, then print the new message.
        let flush_count = dedup.count;
        let flush_tag = dedup.last_tag;
        dedup.last_hash = hash;
        dedup.last_tag = tag;
        dedup.count = 0;
        drop(dedup);

        if let Ok(writer) = self.writer.read() {
            if flush_count > 0 {
                let repeat_line = format!("↑ repeated {flush_count}×");
                writer(flush_tag, &repeat_line);
                if let Some(ref buf) = self.log_buffer {
                    buf.push(format!("{flush_tag}: {repeat_line}"));
                }
            }
            writer(tag, &visitor.0);
            if let Some(ref buf) = self.log_buffer {
                buf.push(format!("{tag}: {}", visitor.0));
            }
        }
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
