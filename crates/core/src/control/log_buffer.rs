//! Bounded ring buffer for recent daemon log lines.
//!
//! Stores the last `capacity` formatted log lines in a `VecDeque` behind
//! `Arc<Mutex<_>>`. Writers (the tracing layer) push lines; readers
//! (IPC/REST/MCP) snapshot the tail.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Default number of log lines to retain.
const DEFAULT_CAPACITY: usize = 200;

/// Thread-safe handle to a bounded log ring buffer.
#[derive(Debug, Clone)]
pub struct LogBuffer(Arc<Mutex<VecDeque<String>>>);

impl LogBuffer {
    /// Create a new buffer that retains at most `DEFAULT_CAPACITY` lines.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::with_capacity(
            DEFAULT_CAPACITY,
        ))))
    }

    /// Append a formatted log line, evicting the oldest if at capacity.
    pub fn push(&self, line: String) {
        let Ok(mut buf) = self.0.lock() else {
            return;
        };
        if buf.len() >= DEFAULT_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    /// Return the most recent `count` lines (or all if `count` is 0).
    #[must_use]
    pub fn tail(&self, count: u32) -> Vec<String> {
        let Ok(buf) = self.0.lock() else {
            return Vec::new();
        };
        if count == 0 || count as usize >= buf.len() {
            buf.iter().cloned().collect()
        } else {
            buf.iter()
                .skip(buf.len() - count as usize)
                .cloned()
                .collect()
        }
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_most_recent_lines() {
        let buf = LogBuffer::new();
        for i in 0..10 {
            buf.push(format!("line {i}"));
        }
        let tail = buf.tail(3);
        assert_eq!(tail, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn tail_zero_returns_all() {
        let buf = LogBuffer::new();
        for i in 0..5 {
            buf.push(format!("line {i}"));
        }
        assert_eq!(buf.tail(0).len(), 5);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let buf = LogBuffer::new();
        for i in 0..250 {
            buf.push(format!("line {i}"));
        }
        let all = buf.tail(0);
        assert_eq!(all.len(), DEFAULT_CAPACITY);
        assert_eq!(all[0], "line 50");
        assert_eq!(all[DEFAULT_CAPACITY - 1], "line 249");
    }
}
