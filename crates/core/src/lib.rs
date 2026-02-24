#![feature(ip_as_octets)]
#![warn(unused_extern_crates)]
#![warn(clippy::pedantic)]

mod tls;
pub mod types;

pub mod client;
pub mod control;
pub mod daemon;
pub mod entry;
pub mod exit;
pub mod ipc;
pub mod node_api;
pub mod server;
pub mod transport;

use client::config::ClientConfig;
use server::config::ServerConfig;
pub use types::{Cidr, CidrParseError, NodeRole, normalize_socket_addr};

#[cfg(test)]
pub mod pcap_parser;

#[cfg(test)]
pub mod test_helpers;
