#![feature(ip_from)]
#![feature(ip_as_octets)]
#![feature(maybe_uninit_slice)]
#![warn(clippy::pedantic)]
// I will decide how many lines is good or bad during dev
#![allow(clippy::too_many_lines)]
// missing errors doc is fine during rapid dev period
#![allow(clippy::missing_errors_doc)]

// mod channel;
mod tls;

// public
pub mod agent;
pub mod client;
pub mod host;
pub mod server;

// re-exports
use client::config::ClientConfig;
use server::config::ServerConfig;

#[cfg(test)]
pub mod pcap_parser;

#[cfg(test)]
pub mod test_helpers;
