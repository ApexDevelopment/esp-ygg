//! What is known about other nodes from asking for their nodeinfo: their
//! names and whatever else they share. Lookups go a few at a time, so they
//! do not crowd the radio, and those that go unanswered are retried.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use yggdrasil_espnow::PublicKey;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

/// Nodeinfo requests in flight at once.
const MAX_LOOKUPS: usize = 3;
/// Lookups that time out are retried this many times.
const LOOKUP_ATTEMPTS: u8 = 3;

#[derive(Clone, Debug)]
pub enum Lookup {
	Pending {
		attempt: u8,
	},
	Failed,
	/// The nodeinfo fields, in order, with the name if it has one.
	Done {
		name: Option<String>,
		fields: Vec<(String, String)>,
	},
}

#[derive(Default)]
pub struct Directory {
	lookups: BTreeMap<PublicKey, Lookup>,
}

impl Directory {
	pub fn new() -> Self {
		Self::default()
	}

	/// Starts looking up nodes from `wanted` that have not been asked
	/// about, as far as there is room for more lookups in flight. Returns
	/// the nodes to ask for their nodeinfo.
	pub fn start_lookups(&mut self, wanted: impl IntoIterator<Item = PublicKey>) -> Vec<PublicKey> {
		let pending = self
			.lookups
			.values()
			.filter(|lookup| matches!(lookup, Lookup::Pending { .. }))
			.count();
		let new: Vec<PublicKey> = wanted
			.into_iter()
			.filter(|key| !self.lookups.contains_key(key))
			.take(MAX_LOOKUPS.saturating_sub(pending))
			.collect();
		for &key in &new {
			self.lookups.insert(key, Lookup::Pending { attempt: 0 });
		}
		new
	}

	/// Takes a node's nodeinfo, or `None` if it did not answer in time.
	/// Returns whether to ask it again.
	pub fn handle_nodeinfo(&mut self, key: PublicKey, info: Option<&[u8]>) -> bool {
		let attempt = match self.lookups.get(&key) {
			Some(Lookup::Pending { attempt }) => *attempt,
			_ => return false,
		};
		let (lookup, again) = match info.and_then(parse_nodeinfo) {
			Some((name, fields)) => (Lookup::Done { name, fields }, false),
			None if info.is_none() && attempt + 1 < LOOKUP_ATTEMPTS => (
				Lookup::Pending {
					attempt: attempt + 1,
				},
				true,
			),
			None => (Lookup::Failed, false),
		};
		self.lookups.insert(key, lookup);
		again
	}

	pub fn lookup(&self, key: &PublicKey) -> Option<&Lookup> {
		self.lookups.get(key)
	}

	pub fn name_of(&self, key: &PublicKey) -> Option<&str> {
		match self.lookups.get(key) {
			Some(Lookup::Done {
				name: Some(name), ..
			}) => Some(name),
			_ => None,
		}
	}

	/// The name of the node at `address`, if it has been looked up.
	pub fn name_of_address(&self, address: &Ipv6Addr) -> Option<&str> {
		self.lookups
			.keys()
			.find(|key| Ipv6Addr::from(addr_for_key(key).0) == *address)
			.and_then(|key| self.name_of(key))
	}

	/// A node's name if known, or its address.
	pub fn label(&self, key: &PublicKey) -> String {
		self.name_of(key)
			.map(String::from)
			.unwrap_or_else(|| Ipv6Addr::from(addr_for_key(key).0).to_string())
	}
}

type NodeInfoFields = (Option<String>, Vec<(String, String)>);

fn parse_nodeinfo(json: &[u8]) -> Option<NodeInfoFields> {
	let info: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(json).ok()?;
	let name = info
		.get("name")
		.and_then(|name| name.as_str())
		.map(String::from);
	let fields = info
		.into_iter()
		.map(|(key, value)| {
			let value = match value {
				serde_json::Value::String(text) => text,
				other => other.to_string(),
			};
			(key, value)
		})
		.collect();
	Some((name, fields))
}
