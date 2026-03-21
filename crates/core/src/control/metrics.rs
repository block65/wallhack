use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::node_api;

#[derive(Debug, Default)]
pub struct Metrics {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    packets_in: AtomicU64,
    packets_out: AtomicU64,
    active_connections: AtomicU64,
    active_flows: AtomicU64,
    packets_dropped: AtomicU64,
    /// Monotonically increasing count of all connections ever opened.
    total_connections: AtomicU64,
    /// Monotonically increasing count of all flows ever opened.
    total_flows: AtomicU64,
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> node_api::Metrics {
        node_api::Metrics {
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            packets_in: self.packets_in.load(Ordering::Relaxed),
            packets_out: self.packets_out.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            active_flows: self.active_flows.load(Ordering::Relaxed),
            packets_dropped: self.packets_dropped.load(Ordering::Relaxed),
            total_connections: self.total_connections.load(Ordering::Relaxed),
            total_flows: self.total_flows.load(Ordering::Relaxed),
        }
    }

    pub fn inc_bytes_in(&self, count: u64) {
        self.bytes_in.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_bytes_out(&self, count: u64) {
        self.bytes_out.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_packets_in(&self, count: u64) {
        self.packets_in.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_packets_out(&self, count: u64) {
        self.packets_out.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_active_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn inc_packets_dropped(&self, count: u64) {
        self.packets_dropped.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_active_flows(&self) {
        self.active_flows.fetch_add(1, Ordering::Relaxed);
        self.total_flows.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_flows(&self) {
        self.active_flows.fetch_sub(1, Ordering::Relaxed);
    }
}

pub type SharedMetrics = Arc<Metrics>;

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn total_connections_increments_and_does_not_decrement() {
        let metrics = Metrics::new();

        metrics.inc_active_connections();
        assert_eq!(metrics.snapshot().total_connections, 1);
        assert_eq!(metrics.snapshot().active_connections, 1);

        metrics.dec_active_connections();
        // active_connections decrements, total_connections must not
        assert_eq!(metrics.snapshot().total_connections, 1);
        assert_eq!(metrics.snapshot().active_connections, 0);
    }

    #[test]
    fn total_flows_increments_and_does_not_decrement() {
        let metrics = Metrics::new();

        metrics.inc_active_flows();
        assert_eq!(metrics.snapshot().total_flows, 1);
        assert_eq!(metrics.snapshot().active_flows, 1);

        metrics.dec_active_flows();
        // active_flows decrements, total_flows must not
        assert_eq!(metrics.snapshot().total_flows, 1);
        assert_eq!(metrics.snapshot().active_flows, 0);
    }

    #[test]
    fn cumulative_counters_survive_connection_churn() {
        let metrics = Metrics::new();

        // Simulate 5 connection open/close cycles
        for _ in 0..5 {
            metrics.inc_active_connections();
            metrics.dec_active_connections();
        }

        assert_eq!(metrics.snapshot().total_connections, 5);
        assert_eq!(metrics.snapshot().active_connections, 0);

        // Simulate 3 flow open/close cycles
        for _ in 0..3 {
            metrics.inc_active_flows();
            metrics.dec_active_flows();
        }

        assert_eq!(metrics.snapshot().total_flows, 3);
        assert_eq!(metrics.snapshot().active_flows, 0);
    }
}
