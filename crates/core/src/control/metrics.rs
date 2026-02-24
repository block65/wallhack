use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Default)]
pub struct Metrics {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub active_connections: AtomicU64,
    pub active_flows: AtomicU64,
    pub packets_dropped: AtomicU64,
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    }

    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn inc_packets_dropped(&self, count: u64) {
        self.packets_dropped.fetch_add(count, Ordering::Relaxed);
    }

    pub fn inc_active_flows(&self) {
        self.active_flows.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_flows(&self) {
        self.active_flows.fetch_sub(1, Ordering::Relaxed);
    }
}

pub type SharedMetrics = Arc<Metrics>;
