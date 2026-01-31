#![feature(ip_as_octets)]
#![warn(unused_extern_crates)]
#![warn(clippy::pedantic)]
// I will decide how many lines is good or bad during dev
#![allow(clippy::too_many_lines)]
// missing errors doc is fine during rapid dev period
#![allow(clippy::missing_errors_doc)]

// mod channel;
mod tls;
pub mod types;

// public
pub mod client;
pub mod control;
pub mod entry;
pub mod exit;
pub mod server;
pub mod transport;

// re-exports
use client::config::ClientConfig;
use server::config::ServerConfig;
pub use types::NodeRole;

#[cfg(test)]
pub mod pcap_parser;

#[cfg(test)]
pub mod test_helpers;
