//! The on-device user interface. [`App`] holds the state and turns key
//! presses into [`Command`]s for the host to carry out; [`draw`] renders it
//! with Ratatui. Another renderer could replace [`draw`] without touching
//! the rest.

mod input;
mod view;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::time::Duration;

use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Instant, Node, Platform, PublicKey};

use crate::BoardInfo;
use crate::overlay::{CONSOLE_PORT, Event};
use crate::settings::Settings;
pub use input::TextInput;
pub use view::draw;

const PING_INTERVAL: Duration = Duration::from_secs(1);
const LOG_LINES: usize = 64;
/// Nodeinfo requests in flight at once, so lookups do not crowd the radio.
const MAX_LOOKUPS: usize = 3;
/// Lookups that time out are retried this many times.
const LOOKUP_ATTEMPTS: u8 = 3;
const MAX_MESSAGE: usize = 200;

/// A key press, after the keyboard driver has applied Shift and Fn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
	Char(char),
	Enter,
	Backspace,
	Tab,
	Escape,
	Up,
	Down,
	Left,
	Right,
}

/// Something the host should do for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
	Ping(Ipv6Addr),
	SendDatagram(SocketAddrV6, Vec<u8>),
	RequestNodeInfo(PublicKey),
	ScanWifi,
	/// Store these settings and restart to apply them.
	Save(Settings),
}

/// The state of the bridge to the wider network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Uplink {
	#[default]
	Off,
	JoiningWifi,
	Connecting,
	Connected,
}

/// What the home screen shows, refreshed by the host every second or so.
#[derive(Clone, Debug)]
pub struct Status {
	pub address: Ipv6Addr,
	pub public_key: PublicKey,
	/// ESP-NOW neighbors that finished the handshake.
	pub neighbors: usize,
	/// Every direct peer, over ESP-NOW or the bridge.
	pub peers: Vec<PublicKey>,
	/// Every node in this node's view of the network.
	pub known: Vec<PublicKey>,
	pub uplink: Uplink,
	pub board: BoardInfo,
}

impl Status {
	pub fn from_node<P: Platform>(node: &Node<P>, board: &BoardInfo, uplink: Uplink) -> Self {
		let debug = node.network().debug();
		let own = node.public_key();
		Status {
			address: Ipv6Addr::from(node.address().0),
			public_key: own,
			neighbors: node.neighbors().filter(|(_, key)| key.is_some()).count(),
			peers: debug.get_peers().into_iter().map(|peer| peer.key).collect(),
			known: debug
				.get_tree()
				.into_iter()
				.map(|entry| entry.key)
				.filter(|key| *key != own)
				.collect(),
			uplink,
			board: board.clone(),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
	pub ssid: String,
	/// In dBm.
	pub signal: i8,
	pub secured: bool,
}

#[derive(Clone, Debug)]
enum Lookup {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
	Home,
	Network,
	Node(PublicKey),
	Ping,
	Console,
	Settings,
	Networks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
	Name,
	Bridge,
	Network,
	Password,
	Peer,
	Channel,
	Save,
}

const FIELDS: [Field; 7] = [
	Field::Name,
	Field::Bridge,
	Field::Network,
	Field::Password,
	Field::Peer,
	Field::Channel,
	Field::Save,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleFocus {
	Target,
	Message,
}

const MENU: [(&str, Screen); 4] = [
	("Network", Screen::Network),
	("Ping", Screen::Ping),
	("Console", Screen::Console),
	("Settings", Screen::Settings),
];

pub struct App {
	now: Instant,
	status: Option<Status>,
	screen: Screen,
	menu: usize,
	commands: VecDeque<Command>,
	/// A short message about the last action, cleared by the next key.
	notice: Option<String>,

	lookups: BTreeMap<PublicKey, Lookup>,
	network_selected: usize,

	ping_target: TextInput,
	pinging: bool,
	last_ping: Option<Instant>,
	ping_log: VecDeque<String>,
	ping_sent: u32,
	ping_received: u32,
	ping_total: Duration,

	console_target: TextInput,
	console_message: TextInput,
	console_focus: ConsoleFocus,
	console_log: VecDeque<String>,
	unread: usize,

	saved: Settings,
	draft: Settings,
	settings_selected: usize,
	editing: Option<(Field, TextInput)>,
	networks: Option<Vec<WifiNetwork>>,
	networks_selected: usize,
}

impl App {
	pub fn new(settings: Settings, now: Instant) -> Self {
		App {
			now,
			status: None,
			screen: Screen::Home,
			menu: 0,
			commands: VecDeque::new(),
			notice: None,
			lookups: BTreeMap::new(),
			network_selected: 0,
			ping_target: TextInput::new("", 45),
			pinging: false,
			last_ping: None,
			ping_log: VecDeque::new(),
			ping_sent: 0,
			ping_received: 0,
			ping_total: Duration::ZERO,
			console_target: TextInput::new("", 52),
			console_message: TextInput::new("", MAX_MESSAGE),
			console_focus: ConsoleFocus::Target,
			console_log: VecDeque::new(),
			unread: 0,
			draft: settings.clone(),
			saved: settings,
			settings_selected: 0,
			editing: None,
			networks: None,
			networks_selected: 0,
		}
	}

	/// Takes the next thing the host should do.
	pub fn poll_command(&mut self) -> Option<Command> {
		self.commands.pop_front()
	}

	pub fn set_status(&mut self, status: Status) {
		self.status = Some(status);
	}

	pub fn set_networks(&mut self, mut networks: Vec<WifiNetwork>) {
		networks.sort_by_key(|network| core::cmp::Reverse(network.signal));
		networks.dedup_by(|a, b| a.ssid == b.ssid);
		self.networks = Some(networks);
		self.networks_selected = 0;
	}

	/// Runs timers: repeated pings and nodeinfo lookups.
	pub fn tick(&mut self, now: Instant) {
		self.now = now;
		if self.pinging
			&& self
				.last_ping
				.is_none_or(|last| now >= last + PING_INTERVAL)
			&& let Ok(target) = self.ping_target.text.parse()
		{
			self.commands.push_back(Command::Ping(target));
			self.last_ping = Some(now);
			self.ping_sent += 1;
		}
		if matches!(self.screen, Screen::Network | Screen::Node(_)) {
			self.start_lookups();
		}
	}

	pub fn handle_event(&mut self, event: Event) {
		match event {
			Event::PingReply {
				from,
				sequence,
				round_trip,
			} => {
				if self.ping_target.text.parse() == Ok(from) {
					self.ping_received += 1;
					self.ping_total += round_trip;
					self.log_ping(format!("#{sequence}  {} ms", round_trip.as_millis()));
				}
			}
			Event::PingTimeout { target, sequence } => {
				if self.ping_target.text.parse() == Ok(target) {
					self.log_ping(format!("#{sequence}  timed out"));
				}
			}
			Event::Datagram { from, data } => {
				let text = String::from_utf8_lossy(&data);
				let line = format!(
					"{} {}",
					self.short_name_of_address(from.ip()),
					text.trim_end()
				);
				push_limited(&mut self.console_log, line);
				if self.screen != Screen::Console {
					self.unread += 1;
				}
			}
		}
	}

	pub fn handle_nodeinfo(&mut self, key: PublicKey, info: Option<Vec<u8>>) {
		let attempt = match self.lookups.get(&key) {
			Some(Lookup::Pending { attempt }) => *attempt,
			_ => return,
		};
		let lookup = match info.as_deref().and_then(parse_nodeinfo) {
			Some((name, fields)) => Lookup::Done { name, fields },
			None if info.is_none() && attempt + 1 < LOOKUP_ATTEMPTS => {
				self.commands.push_back(Command::RequestNodeInfo(key));
				Lookup::Pending {
					attempt: attempt + 1,
				}
			}
			None => Lookup::Failed,
		};
		self.lookups.insert(key, lookup);
	}

	pub fn handle_key(&mut self, key: Key) {
		self.notice = None;
		match self.screen {
			Screen::Home => self.home_key(key),
			Screen::Network => self.network_key(key),
			Screen::Node(node) => self.node_key(node, key),
			Screen::Ping => self.ping_key(key),
			Screen::Console => self.console_key(key),
			Screen::Settings => self.settings_key(key),
			Screen::Networks => self.networks_key(key),
		}
	}

	fn home_key(&mut self, key: Key) {
		match navigation(key) {
			Some(Key::Up) => self.menu = self.menu.saturating_sub(1),
			Some(Key::Down) => self.menu = (self.menu + 1).min(MENU.len() - 1),
			Some(Key::Enter | Key::Right) => self.open(MENU[self.menu].1),
			_ => {}
		}
	}

	fn open(&mut self, screen: Screen) {
		if screen == Screen::Console {
			self.unread = 0;
		}
		if screen == Screen::Settings {
			self.draft = self.saved.clone();
			self.settings_selected = 0;
		}
		self.screen = screen;
	}

	fn network_key(&mut self, key: Key) {
		let count = self.nodes().len();
		match navigation(key) {
			Some(Key::Up) => self.network_selected = self.network_selected.saturating_sub(1),
			Some(Key::Down) => {
				self.network_selected = (self.network_selected + 1).min(count.saturating_sub(1))
			}
			Some(Key::Enter | Key::Right) => {
				if let Some(&node) = self.nodes().get(self.network_selected) {
					self.screen = Screen::Node(node);
				}
			}
			Some(Key::Escape | Key::Left) => self.screen = Screen::Home,
			_ => {}
		}
	}

	fn node_key(&mut self, node: PublicKey, key: Key) {
		let address = Ipv6Addr::from(addr_for_key(&node).0);
		match key {
			Key::Char('p') => {
				self.ping_target = TextInput::new(&address.to_string(), 45);
				self.start_pinging();
				self.screen = Screen::Ping;
			}
			Key::Char('m') => {
				self.console_target = TextInput::new(&address.to_string(), 52);
				self.console_focus = ConsoleFocus::Message;
				self.open(Screen::Console);
			}
			_ if matches!(navigation(key), Some(Key::Escape | Key::Left)) => {
				self.screen = Screen::Network
			}
			_ => {}
		}
	}

	fn ping_key(&mut self, key: Key) {
		match key {
			Key::Enter if self.pinging => self.pinging = false,
			Key::Enter => {
				if self.ping_target.text.parse::<Ipv6Addr>().is_ok() {
					self.start_pinging();
				} else {
					self.notice = Some("Not an IPv6 address".into());
				}
			}
			Key::Escape => {
				self.pinging = false;
				self.screen = Screen::Home;
			}
			Key::Backspace if !self.pinging => self.ping_target.backspace(),
			Key::Char(character) if !self.pinging => self.ping_target.push(character),
			_ => {}
		}
	}

	fn start_pinging(&mut self) {
		self.pinging = true;
		self.last_ping = None;
		self.ping_log.clear();
		self.ping_sent = 0;
		self.ping_received = 0;
		self.ping_total = Duration::ZERO;
	}

	fn log_ping(&mut self, line: String) {
		push_limited(&mut self.ping_log, line);
	}

	fn console_key(&mut self, key: Key) {
		let field = match self.console_focus {
			ConsoleFocus::Target => &mut self.console_target,
			ConsoleFocus::Message => &mut self.console_message,
		};
		match key {
			Key::Escape => self.screen = Screen::Home,
			Key::Tab | Key::Up | Key::Down => {
				self.console_focus = match self.console_focus {
					ConsoleFocus::Target => ConsoleFocus::Message,
					ConsoleFocus::Message => ConsoleFocus::Target,
				}
			}
			Key::Enter => match self.console_focus {
				ConsoleFocus::Target => self.console_focus = ConsoleFocus::Message,
				ConsoleFocus::Message => self.send_console_message(),
			},
			Key::Backspace => field.backspace(),
			Key::Char(character) => field.push(character),
			_ => {}
		}
	}

	fn send_console_message(&mut self) {
		let Some(to) = parse_destination(&self.console_target.text) else {
			self.notice = Some("Enter an address, or [address]:port".into());
			self.console_focus = ConsoleFocus::Target;
			return;
		};
		if self.console_message.text.is_empty() {
			return;
		}
		let mut data = self.console_message.text.clone().into_bytes();
		data.push(b'\n');
		self.commands.push_back(Command::SendDatagram(to, data));
		let line = format!("> {}", self.console_message.text);
		push_limited(&mut self.console_log, line);
		self.console_message.clear();
	}

	fn settings_key(&mut self, key: Key) {
		if let Some((field, input)) = &mut self.editing {
			match key {
				Key::Enter => {
					let (field, input) = (*field, input.text.clone());
					self.apply_field(field, input);
					self.editing = None;
				}
				Key::Escape => self.editing = None,
				Key::Backspace => input.backspace(),
				Key::Char(character) => input.push(character),
				_ => {}
			}
			return;
		}
		match navigation(key) {
			Some(Key::Up) => self.settings_selected = self.settings_selected.saturating_sub(1),
			Some(Key::Down) => {
				self.settings_selected = (self.settings_selected + 1).min(FIELDS.len() - 1)
			}
			Some(Key::Escape | Key::Left) => self.screen = Screen::Home,
			Some(Key::Enter | Key::Right) => self.activate(FIELDS[self.settings_selected]),
			_ => {}
		}
	}

	fn activate(&mut self, field: Field) {
		let input = match field {
			Field::Name => TextInput::new(&self.draft.name, 32),
			Field::Password => TextInput::secret(&self.draft.password, 63),
			Field::Peer => TextInput::new(&self.draft.peer, 21),
			Field::Channel => TextInput::new(&self.draft.channel.to_string(), 2),
			Field::Bridge => {
				self.draft.bridge = !self.draft.bridge;
				return;
			}
			Field::Network => {
				self.networks = None;
				self.commands.push_back(Command::ScanWifi);
				self.screen = Screen::Networks;
				return;
			}
			Field::Save => {
				if self.draft.bridge && !self.draft.can_bridge() {
					self.notice = Some("Bridging needs a network and peer".into());
					return;
				}
				self.saved = self.draft.clone();
				self.commands.push_back(Command::Save(self.draft.clone()));
				self.notice = Some("Saved, restarting".into());
				return;
			}
		};
		self.editing = Some((field, input));
	}

	fn apply_field(&mut self, field: Field, text: String) {
		match field {
			Field::Name => self.draft.name = text,
			Field::Password => self.draft.password = text,
			Field::Peer => {
				if text.is_empty() || text.parse::<core::net::SocketAddrV4>().is_ok() {
					self.draft.peer = text;
				} else {
					self.notice = Some("Use an IPv4 address and port".into());
				}
			}
			Field::Channel => match text.parse::<u8>() {
				Ok(channel @ 1..=13) => self.draft.channel = channel,
				_ => self.notice = Some("Channels go from 1 to 13".into()),
			},
			Field::Bridge | Field::Network | Field::Save => {}
		}
	}

	fn networks_key(&mut self, key: Key) {
		let count = self.networks.as_ref().map_or(0, Vec::len);
		match navigation(key) {
			Some(Key::Up) => self.networks_selected = self.networks_selected.saturating_sub(1),
			Some(Key::Down) => {
				self.networks_selected = (self.networks_selected + 1).min(count.saturating_sub(1))
			}
			Some(Key::Enter | Key::Right) => {
				if let Some(network) = self
					.networks
					.as_ref()
					.and_then(|networks| networks.get(self.networks_selected))
				{
					self.draft.ssid = network.ssid.clone();
					if !network.secured {
						self.draft.password.clear();
					}
					self.screen = Screen::Settings;
				}
			}
			Some(Key::Escape | Key::Left) => self.screen = Screen::Settings,
			Some(Key::Char('r')) => {
				self.networks = None;
				self.commands.push_back(Command::ScanWifi);
			}
			_ => {}
		}
	}

	/// Known nodes, direct peers first, then by address.
	fn nodes(&self) -> Vec<PublicKey> {
		let Some(status) = &self.status else {
			return Vec::new();
		};
		let mut nodes = status.known.clone();
		for peer in &status.peers {
			if !nodes.contains(peer) && *peer != status.public_key {
				nodes.push(*peer);
			}
		}
		nodes.sort_by_key(|key| (!status.peers.contains(key), addr_for_key(key).0));
		nodes
	}

	fn start_lookups(&mut self) {
		let pending = self
			.lookups
			.values()
			.filter(|lookup| matches!(lookup, Lookup::Pending { .. }))
			.count();
		let wanted: Vec<PublicKey> = match self.screen {
			Screen::Node(node) => Vec::from([node]),
			_ => self.nodes(),
		};
		let new: Vec<PublicKey> = wanted
			.into_iter()
			.filter(|key| !self.lookups.contains_key(key))
			.take(MAX_LOOKUPS.saturating_sub(pending))
			.collect();
		for key in new {
			self.lookups.insert(key, Lookup::Pending { attempt: 0 });
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
	}

	fn name_of(&self, key: &PublicKey) -> Option<&str> {
		match self.lookups.get(key) {
			Some(Lookup::Done {
				name: Some(name), ..
			}) => Some(name),
			_ => None,
		}
	}

	/// A node's name if known, or its address.
	fn short_name_of_address(&self, address: &Ipv6Addr) -> String {
		self.lookups
			.iter()
			.find(|(key, _)| Ipv6Addr::from(addr_for_key(key).0) == *address)
			.and_then(|(key, _)| self.name_of(key))
			.map_or_else(|| address.to_string(), String::from)
	}
}

/// Lets the bare `;` `.` `,` and `/` keys, which carry the arrows on the
/// Cardputer, move around lists without holding Fn, and `` ` `` go back.
fn navigation(key: Key) -> Option<Key> {
	Some(match key {
		Key::Char(';') => Key::Up,
		Key::Char('.') => Key::Down,
		Key::Char(',') => Key::Left,
		Key::Char('/') => Key::Right,
		Key::Char('`') => Key::Escape,
		other => other,
	})
}

/// `[address]:port`, or a bare address for the console port.
fn parse_destination(text: &str) -> Option<SocketAddrV6> {
	text.parse::<SocketAddrV6>().ok().or_else(|| {
		let address: Ipv6Addr = text.parse().ok()?;
		Some(SocketAddrV6::new(address, CONSOLE_PORT, 0, 0))
	})
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

fn push_limited(log: &mut VecDeque<String>, line: String) {
	if log.len() == LOG_LINES {
		log.pop_front();
	}
	log.push_back(line);
}

#[cfg(test)]
mod tests;
