//! Keeps the interface showing what the node reports, and asks for the
//! names of the nodes on the page being looked at.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use esp_ygg_app::time::short_uptime;
use esp_ygg_app::ui::directory::Directory;
use esp_ygg_app::ui::{Command, Status};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use yggdrasil_espnow::PublicKey;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

use crate::{MainWindow, NeighborEntry, Node, NodeEntry};

/// The pages whose names are looked up, as the navigation numbers them.
const NODES_PAGE: i32 = 1;
const NEIGHBORS_PAGE: i32 = 2;

pub struct Gui {
	window: MainWindow,
	directory: Directory,
	status: Option<Status>,
	nodes: Rc<VecModel<NodeEntry>>,
	neighbors: Rc<VecModel<NeighborEntry>>,
	/// What the host has been asked to do, oldest first.
	commands: VecDeque<Command>,
}

impl Gui {
	pub fn new() -> Result<Self, slint::PlatformError> {
		let window = MainWindow::new()?;
		let nodes = Rc::new(VecModel::default());
		let neighbors = Rc::new(VecModel::default());
		let node = window.global::<Node>();
		node.set_nodes(ModelRc::from(nodes.clone()));
		node.set_neighbors(ModelRc::from(neighbors.clone()));
		Ok(Gui {
			window,
			directory: Directory::new(),
			status: None,
			nodes,
			neighbors,
			commands: VecDeque::new(),
		})
	}

	pub fn window(&self) -> &MainWindow {
		&self.window
	}

	pub fn set_status(&mut self, status: Status) {
		let node = self.window.global::<Node>();
		if !status.board.name.is_empty() {
			node.set_name(status.board.name.as_str().into());
		}
		node.set_address(status.address.to_string().into());
		node.set_neighbor_count(status.connected_neighbors() as i32);
		node.set_peer_count(status.peers.len() as i32);
		node.set_known_count(status.known.len() as i32);
		node.set_uptime(short_uptime(status.board.uptime.as_secs()).into());
		let free = status.board.free_heap.unwrap_or_default() / 1024;
		node.set_free_memory(free as i32);
		self.status = Some(status);
		self.refresh_lists();
	}

	/// Takes a node's nodeinfo, or `None` if it did not answer in time.
	pub fn handle_nodeinfo(&mut self, key: PublicKey, info: Option<&[u8]>) {
		if self.directory.handle_nodeinfo(key, info) {
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
		self.refresh_lists();
	}

	/// Looks up the names of nodes on the page showing.
	pub fn tick(&mut self) {
		let Some(status) = &self.status else {
			return;
		};
		let wanted: Vec<PublicKey> = match self.window.get_page() {
			NODES_PAGE => status.nodes(),
			NEIGHBORS_PAGE => status.neighbors.iter().filter_map(|n| n.key).collect(),
			_ => Vec::new(),
		};
		for key in self.directory.start_lookups(wanted) {
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
	}

	pub fn poll_command(&mut self) -> Option<Command> {
		self.commands.pop_front()
	}

	fn refresh_lists(&self) {
		let Some(status) = &self.status else {
			return;
		};
		let nodes = status
			.nodes()
			.into_iter()
			.map(|key| self.node_entry(&key, status))
			.collect();
		sync(&self.nodes, nodes);
		let neighbors = status
			.neighbors
			.iter()
			.map(|neighbor| NeighborEntry {
				name: match neighbor.key {
					Some(key) => self.directory.label(&key),
					None => neighbor.mac_text(),
				}
				.into(),
				has_signal: neighbor.signal.is_some(),
				bars: neighbor.signal_bars().unwrap_or_default() as i32,
				signal: neighbor.signal.map(i32::from).unwrap_or_default(),
				loss: neighbor.loss.map_or(-1, i32::from),
				linking: neighbor.key.is_none(),
			})
			.collect();
		sync(&self.neighbors, neighbors);
	}

	fn node_entry(&self, key: &PublicKey, status: &Status) -> NodeEntry {
		let address = Ipv6Addr::from(addr_for_key(key).0).to_string();
		let (name, address) = match self.directory.name_of(key) {
			Some(name) => (name.into(), address.into()),
			None => (address.into(), Default::default()),
		};
		NodeEntry {
			name,
			address,
			peer: status.peers.contains(key),
		}
	}
}

/// Changes `model` to hold `rows`, touching only rows that differ, so the
/// lists keep their place and only what changed is drawn again.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
	for (index, row) in rows.iter().enumerate() {
		if index >= model.row_count() {
			model.push(row.clone());
		} else if model.row_data(index).as_ref() != Some(row) {
			model.set_row_data(index, row.clone());
		}
	}
	while model.row_count() > rows.len() {
		model.remove(model.row_count() - 1);
	}
}
