#![feature(ip_as_octets)]
#![warn(unused_extern_crates)]
#![warn(clippy::pedantic)]
// These functions are internal APIs where error types are self-documenting;
// maintaining # Errors sections on every Result-returning fn is not worth the overhead.
#![allow(clippy::missing_errors_doc)]

mod tls;
pub mod types;

pub mod client;
pub mod control;
pub mod daemon;
pub mod entry;
pub mod exit;
pub mod hmac;
pub mod ipc;
pub mod node_api;
pub mod psk;
pub mod server;
pub mod transport;

use client::config::ClientConfig;
use server::config::ServerConfig;
pub use types::{Cidr, CidrParseError, NodeRole, NodeRoleError, SocketAddrExt};

#[cfg(test)]
pub mod pcap_parser;

#[cfg(test)]
pub mod test_helpers;
