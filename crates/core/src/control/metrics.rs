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
