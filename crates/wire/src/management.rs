// Suppress clippy warnings from auto-generated prost code
#[allow(
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/wallhack.management.rs"));
}
pub use generated::*;

use std::fmt;

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry => f.write_str("entry"),
            Self::Exit => f.write_str("exit"),
            Self::Relay => f.write_str("relay"),
            Self::Indeterminate => f.write_str("indeterminate"),
            Self::Unspecified => f.write_str("unknown"),
        }
    }
}

impl fmt::Display for StatsResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bytes in: {}\nbytes out: {}\npackets in: {}\npackets out: {}\n\
             connections: {}\nflows: {}\ndropped: {}\ntotal connections: {}\ntotal flows: {}",
            self.bytes_in,
            self.bytes_out,
            self.packets_in,
            self.packets_out,
            self.active_connections,
            self.active_flows,
            self.packets_dropped,
            self.total_connections,
            self.total_flows,
        )
    }
}

impl fmt::Display for PeerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connected => f.write_str("connected"),
            Self::Disconnected => f.write_str("disconnected"),
            Self::Unspecified => f.write_str("unknown"),
        }
    }
}
