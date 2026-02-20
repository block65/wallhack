//! Route table for mapping CIDRs to exit-node peers.

use std::{collections::HashMap, sync::Arc, time::Instant};

use arc_swap::ArcSwap;

use crate::Cidr;

/// A single route entry mapping a CIDR to a peer.
#[derive(Debug, Clone)]
pub struct RouteEntry {
	/// The destination network.
	pub cidr: Cidr,
	/// Name of the peer responsible for this route.
	pub peer: String,
	/// When this route was added.
	pub added_at: Instant,
}

/// Shared route table.
pub type SharedRouteTable = Arc<RouteTable>;

/// Thread-safe route table mapping CIDRs to exit-node peers.
///
/// Uses `ArcSwap` for wait-free reads.
#[derive(Debug)]
pub struct RouteTable {
	routes: ArcSwap<HashMap<Cidr, RouteEntry>>,
}

impl Default for RouteTable {
	fn default() -> Self {
		Self {
			routes: ArcSwap::from_pointee(HashMap::new()),
		}
	}
}

impl RouteTable {
	/// Create a new empty route table.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Create a new shared route table.
	#[must_use]
	pub fn shared() -> SharedRouteTable {
		Arc::new(Self::new())
	}

	/// Add a route. Returns the previous entry if one existed for this CIDR.
	pub fn add(&self, cidr: Cidr, peer: String) -> Option<RouteEntry> {
		let entry = RouteEntry {
			cidr,
			peer,
			added_at: Instant::now(),
		};
		let mut old_entry = None;
		self.routes.rcu(|old| {
			let mut new = (**old).clone();
			old_entry = new.insert(cidr, entry.clone());
			new
		});
		old_entry
	}

	/// Remove a route by CIDR. Returns the removed entry if it existed.
	pub fn remove(&self, cidr: &Cidr) -> Option<RouteEntry> {
		let mut removed = None;
		self.routes.rcu(|old| {
			let mut new = (**old).clone();
			removed = new.remove(cidr);
			new
		});
		removed
	}

	/// Remove all routes pointing at a specific peer. Returns removed entries.
	pub fn remove_by_peer(&self, peer: &str) -> Vec<RouteEntry> {
		let mut removed = Vec::new();
		self.routes.rcu(|old| {
			let mut new = (**old).clone();
			let to_remove: Vec<Cidr> = new
				.iter()
				.filter(|(_, entry)| entry.peer == peer)
				.map(|(cidr, _)| *cidr)
				.collect();
			removed = to_remove
				.into_iter()
				.filter_map(|cidr| new.remove(&cidr))
				.collect();
			new
		});
		removed
	}

	/// Look up a route by CIDR.
	#[must_use]
	pub fn get(&self, cidr: &Cidr) -> Option<RouteEntry> {
		self.routes.load().get(cidr).cloned()
	}

	/// List all routes.
	#[must_use]
	pub fn list(&self) -> Vec<RouteEntry> {
		self.routes.load().values().cloned().collect()
	}

	/// Number of routes in the table.
	#[must_use]
	pub fn count(&self) -> usize {
		self.routes.load().len()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_add_and_get() {
		let table = RouteTable::new();
		let cidr: Cidr = "10.0.0.0/8".parse().unwrap();
		assert!(table.add(cidr, "peer-1".into()).is_none());

		let entry = table.get(&cidr).unwrap();
		assert_eq!(entry.peer, "peer-1");
		assert_eq!(entry.cidr, cidr);
	}

	#[test]
	fn test_add_replaces() {
		let table = RouteTable::new();
		let cidr: Cidr = "10.0.0.0/8".parse().unwrap();

		assert!(table.add(cidr, "peer-1".into()).is_none());
		let old = table.add(cidr, "peer-2".into()).unwrap();
		assert_eq!(old.peer, "peer-1");
		assert_eq!(table.get(&cidr).unwrap().peer, "peer-2");
	}

	#[test]
	fn test_remove() {
		let table = RouteTable::new();
		let cidr: Cidr = "10.0.0.0/8".parse().unwrap();

		table.add(cidr, "peer-1".into());
		assert_eq!(table.count(), 1);

		let removed = table.remove(&cidr).unwrap();
		assert_eq!(removed.peer, "peer-1");
		assert_eq!(table.count(), 0);
		assert!(table.get(&cidr).is_none());
	}

	#[test]
	fn test_remove_by_peer() {
		let table = RouteTable::new();
		let cidr_a: Cidr = "10.0.0.0/8".parse().unwrap();
		let cidr_b: Cidr = "172.16.0.0/12".parse().unwrap();
		let cidr_c: Cidr = "192.168.0.0/16".parse().unwrap();

		table.add(cidr_a, "peer-1".into());
		table.add(cidr_b, "peer-1".into());
		table.add(cidr_c, "peer-2".into());

		let removed = table.remove_by_peer("peer-1");
		assert_eq!(removed.len(), 2);
		assert_eq!(table.count(), 1);
		assert!(table.get(&cidr_a).is_none());
		assert!(table.get(&cidr_b).is_none());
		assert!(table.get(&cidr_c).is_some());
	}

	#[test]
	fn test_list() {
		let table = RouteTable::new();
		let cidr_a: Cidr = "10.0.0.0/8".parse().unwrap();
		let cidr_b: Cidr = "192.168.0.0/16".parse().unwrap();

		table.add(cidr_a, "peer-1".into());
		table.add(cidr_b, "peer-2".into());

		let list = table.list();
		assert_eq!(list.len(), 2);
	}

	#[test]
	fn test_remove_nonexistent() {
		let table = RouteTable::new();
		let cidr: Cidr = "10.0.0.0/8".parse().unwrap();
		assert!(table.remove(&cidr).is_none());
	}

	#[test]
	fn test_remove_by_peer_nonexistent() {
		let table = RouteTable::new();
		let removed = table.remove_by_peer("no-such-peer");
		assert!(removed.is_empty());
	}
}
