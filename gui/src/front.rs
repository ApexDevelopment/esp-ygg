//! Keeps the interface showing what the node reports, asks for the names of
//! the nodes on the page being looked at, and carries out what is asked on
//! the settings page.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::Ipv6Addr;

use esp_ygg_app::Settings;
use esp_ygg_app::settings::TextSetting;
use esp_ygg_app::time::{DateTime, short_uptime};
use esp_ygg_app::ui::directory::Directory;
use esp_ygg_app::ui::{Command, SETTINGS_FILE, Status, WifiNetwork};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use yggdrasil_espnow::PublicKey;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

use crate::{MainWindow, NeighborEntry, Node, NodeEntry, Notice, Setup, WifiEntry};

/// Every setting typed in on the settings page.
const TEXT_SETTINGS: [TextSetting; 8] = [
	TextSetting::Name,
	TextSetting::Channel,
	TextSetting::TimeZone,
	TextSetting::Ssid,
	TextSetting::WifiPassword,
	TextSetting::Peer,
	TextSetting::HotspotName,
	TextSetting::HotspotPassword,
];

/// What a button on the settings page asked for, until the next tick.
#[derive(Clone, Copy)]
enum Action {
	Save,
	Export,
	Import,
	Scan,
}

/// The pages whose names are looked up, as the navigation numbers them.
const NODES_PAGE: i32 = 1;
const NEIGHBORS_PAGE: i32 = 2;

pub struct Gui {
	window: MainWindow,
	directory: Directory,
	status: Option<Status>,
	nodes: Rc<VecModel<NodeEntry>>,
	neighbors: Rc<VecModel<NeighborEntry>>,
	networks: Rc<VecModel<WifiEntry>>,
	/// The settings in use, which edits start from.
	saved: Settings,
	actions: Rc<RefCell<VecDeque<Action>>>,
	/// What the host has been asked to do, oldest first.
	commands: VecDeque<Command>,
}

impl Gui {
	pub fn new(settings: Settings) -> Result<Self, slint::PlatformError> {
		let window = MainWindow::new()?;
		let nodes = Rc::new(VecModel::default());
		let neighbors = Rc::new(VecModel::default());
		let networks = Rc::new(VecModel::default());
		let node = window.global::<Node>();
		node.set_nodes(ModelRc::from(nodes.clone()));
		node.set_neighbors(ModelRc::from(neighbors.clone()));
		let setup = window.global::<Setup>();
		setup.set_networks(ModelRc::from(networks.clone()));
		let actions = Rc::new(RefCell::new(VecDeque::new()));
		let queue = |action| {
			let actions = actions.clone();
			move || actions.borrow_mut().push_back(action)
		};
		setup.on_save(queue(Action::Save));
		setup.on_export(queue(Action::Export));
		setup.on_import(queue(Action::Import));
		setup.on_scan(queue(Action::Scan));
		let gui = Gui {
			window,
			directory: Directory::new(),
			status: None,
			nodes,
			neighbors,
			networks,
			saved: settings,
			actions,
			commands: VecDeque::new(),
		};
		gui.show_settings(&gui.saved);
		Ok(gui)
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

	/// Carries out what the settings page asked for, and looks up the names
	/// of nodes on the page showing.
	pub fn tick(&mut self) {
		let actions: Vec<Action> = self.actions.borrow_mut().drain(..).collect();
		for action in actions {
			self.carry_out(action);
		}
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

	/// Takes the networks a scan found.
	pub fn set_networks(&mut self, mut networks: Vec<WifiNetwork>) {
		networks.sort_by_key(|network| core::cmp::Reverse(network.signal));
		let entries = networks
			.into_iter()
			.map(|network| WifiEntry {
				ssid: network.ssid.into(),
				signal: network.signal.into(),
				secured: network.secured,
			})
			.collect();
		sync(&self.networks, entries);
		self.window.global::<Setup>().set_scanning(false);
	}

	/// How writing settings to the SD card went.
	pub fn settings_exported(&mut self, result: Result<(), String>) {
		self.notify(match result {
			Ok(()) => alloc::format!("Exported to {SETTINGS_FILE}"),
			Err(problem) => problem,
		});
	}

	/// The settings file read from the SD card, or why it could not be.
	/// What it holds replaces what is on the page, until it is saved.
	pub fn settings_imported(&mut self, result: Result<String, String>) {
		let base = self.edited_settings().unwrap_or_else(|| self.saved.clone());
		match result.and_then(|text| base.with_text(&text)) {
			Ok(settings) => {
				self.show_settings(&settings);
				self.notify("Imported, save to apply".into());
			}
			Err(problem) => self.notify(problem),
		}
	}

	/// The local date and time, once the board knows it.
	pub fn local_time(&self) -> Option<DateTime> {
		let time = self.status.as_ref()?.board.time?;
		Some(DateTime::from_unix(
			(time / 1_000_000) as i64,
			self.saved.utc_offset,
		))
	}

	fn carry_out(&mut self, action: Action) {
		match action {
			Action::Save => {
				let Some(settings) = self.edited_settings() else {
					return;
				};
				if settings.bridge && !settings.can_bridge() {
					self.notify("Bridging needs a network and peer".into());
					return;
				}
				self.saved = settings.clone();
				self.commands.push_back(Command::Save(settings));
				self.notify("Saved, restarting".into());
			}
			Action::Export => {
				if let Some(settings) = self.edited_settings() {
					self.commands
						.push_back(Command::ExportSettings(settings.to_text()));
				}
			}
			Action::Import => self.commands.push_back(Command::ImportSettings),
			Action::Scan => {
				sync(&self.networks, Vec::new());
				self.window.global::<Setup>().set_scanning(true);
				self.commands.push_back(Command::ScanWifi);
			}
		}
	}

	fn notify(&self, text: String) {
		self.window.global::<Notice>().set_text(text.into());
	}

	/// Puts `settings` on the settings page.
	fn show_settings(&self, settings: &Settings) {
		let setup = self.window.global::<Setup>();
		for setting in TEXT_SETTINGS {
			set_text(&setup, setting, settings.text(setting).into());
			set_error(&setup, setting, SharedString::new());
		}
		setup.set_status_pages(settings.status_page);
		setup.set_bridge(settings.bridge);
		setup.set_hotspot(settings.hotspot);
	}

	/// The settings as edited on the page, or `None` if some text will not
	/// do, which is then marked on the page.
	fn edited_settings(&self) -> Option<Settings> {
		let setup = self.window.global::<Setup>();
		let mut settings = self.saved.clone();
		settings.status_page = setup.get_status_pages();
		settings.bridge = setup.get_bridge();
		settings.hotspot = setup.get_hotspot();
		let mut valid = true;
		for setting in TEXT_SETTINGS {
			let error = match settings.set_text(setting, &text(&setup, setting)) {
				Ok(()) => "",
				Err(problem) => {
					valid = false;
					problem
				}
			};
			set_error(&setup, setting, error.into());
		}
		if !valid {
			self.notify("Some settings need fixing".into());
		}
		valid.then_some(settings)
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

fn text(setup: &Setup<'_>, setting: TextSetting) -> SharedString {
	match setting {
		TextSetting::Name => setup.get_name(),
		TextSetting::Channel => setup.get_channel(),
		TextSetting::TimeZone => setup.get_time_zone(),
		TextSetting::Ssid => setup.get_ssid(),
		TextSetting::WifiPassword => setup.get_wifi_password(),
		TextSetting::Peer => setup.get_peer(),
		TextSetting::HotspotName => setup.get_hotspot_name(),
		TextSetting::HotspotPassword => setup.get_hotspot_password(),
	}
}

fn set_text(setup: &Setup<'_>, setting: TextSetting, text: SharedString) {
	match setting {
		TextSetting::Name => setup.set_name(text),
		TextSetting::Channel => setup.set_channel(text),
		TextSetting::TimeZone => setup.set_time_zone(text),
		TextSetting::Ssid => setup.set_ssid(text),
		TextSetting::WifiPassword => setup.set_wifi_password(text),
		TextSetting::Peer => setup.set_peer(text),
		TextSetting::HotspotName => setup.set_hotspot_name(text),
		TextSetting::HotspotPassword => setup.set_hotspot_password(text),
	}
}

fn set_error(setup: &Setup<'_>, setting: TextSetting, error: SharedString) {
	match setting {
		TextSetting::Name => setup.set_name_error(error),
		TextSetting::Channel => setup.set_channel_error(error),
		TextSetting::TimeZone => setup.set_time_zone_error(error),
		TextSetting::Ssid => setup.set_ssid_error(error),
		TextSetting::WifiPassword => setup.set_wifi_password_error(error),
		TextSetting::Peer => setup.set_peer_error(error),
		TextSetting::HotspotName => setup.set_hotspot_name_error(error),
		TextSetting::HotspotPassword => setup.set_hotspot_password_error(error),
	}
}
