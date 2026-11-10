//! The on-device user interface. [`App`] holds the state and turns key
//! presses into [`Command`]s for the host to carry out; [`draw`] renders it
//! with Ratatui. Another renderer could replace [`draw`] without touching
//! the rest.

#[cfg(feature = "gemini")]
mod gemini;
mod gopher;
mod input;
mod irc;
pub mod keyboard;
pub mod remote;
mod view;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::time::Duration;

use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Instant, Mac, Node, Platform, PublicKey};

use crate::BoardInfo;
use crate::overlay::{CONSOLE_PORT, Event, Stream};
use crate::settings::Settings;
use crate::time::{format_offset, parse_offset};
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
	/// Send a request to a TCP server and collect its whole response.
	Fetch(SocketAddrV6, Vec<u8>),
	/// Open a lasting TCP connection, send on it, and close it.
	OpenStream(Stream, SocketAddrV6),
	StreamSend(Stream, Vec<u8>),
	CloseStream(Stream),
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
	/// ESP-NOW neighbors, including ones still connecting.
	pub neighbors: Vec<Neighbor>,
	/// Every direct peer, over ESP-NOW or the bridge.
	pub peers: Vec<PublicKey>,
	/// Every node in this node's view of the network.
	pub known: Vec<PublicKey>,
	pub uplink: Uplink,
	/// How many devices are on the hotspot, if it is on.
	pub hotspot: Option<usize>,
	pub board: BoardInfo,
}

impl Status {
	pub fn from_node<P: Platform>(node: &Node<P>, board: &BoardInfo, uplink: Uplink) -> Self {
		let debug = node.network().debug();
		let own = node.public_key();
		Status {
			address: Ipv6Addr::from(node.address().0),
			public_key: own,
			neighbors: node
				.neighbors()
				.map(|(mac, key)| Neighbor {
					mac,
					key,
					signal: None,
					loss: None,
				})
				.collect(),
			peers: debug.get_peers().into_iter().map(|peer| peer.key).collect(),
			known: debug
				.get_tree()
				.into_iter()
				.map(|entry| entry.key)
				.filter(|key| *key != own)
				.collect(),
			uplink,
			hotspot: None,
			board: board.clone(),
		}
	}
}

/// An ESP-NOW neighbor and how well the radio reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbor {
	pub mac: Mac,
	/// Known once the Yggdrasil handshake is done.
	pub key: Option<PublicKey>,
	/// Recent average signal strength of its frames, in dBm.
	pub signal: Option<i8>,
	/// Recent share of frames sent to it that went unacknowledged, in percent.
	pub loss: Option<u8>,
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

/// Nodes whose name or address start with what was typed, for Tab to
/// cycle through.
struct Completion {
	matches: Vec<(Ipv6Addr, Option<String>)>,
	index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
	Home,
	Network,
	Node(PublicKey),
	Radio,
	Ping,
	Console,
	#[cfg_attr(not(feature = "gopher"), allow(dead_code))]
	Gopher,
	#[cfg_attr(not(feature = "irc"), allow(dead_code))]
	Irc,
	#[cfg_attr(not(feature = "gemini"), allow(dead_code))]
	Gemini,
	Settings,
	/// The settings in one group, opened from the main settings.
	SettingsGroup(Group),
	Networks,
}

/// Settings that open as a menu of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
	Bridge,
	Hotspot,
}

impl Group {
	fn fields(self) -> &'static [Field] {
		match self {
			Group::Bridge => &[Field::Bridge, Field::Network, Field::Password, Field::Peer],
			Group::Hotspot => &[Field::Hotspot, Field::HotspotName, Field::HotspotPassword],
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
	Name,
	Channel,
	#[cfg_attr(not(feature = "ntp"), allow(dead_code))]
	TimeZone,
	#[cfg_attr(not(any(feature = "http", feature = "gopher")), allow(dead_code))]
	StatusPage,
	Open(Group),
	Bridge,
	Network,
	Password,
	Peer,
	Hotspot,
	HotspotName,
	HotspotPassword,
	Save,
}

/// The settings, without those for services left out of the build.
const FIELDS: &[Field] = &[
	Field::Name,
	Field::Channel,
	#[cfg(feature = "ntp")]
	Field::TimeZone,
	#[cfg(any(feature = "http", feature = "gopher"))]
	Field::StatusPage,
	Field::Open(Group::Bridge),
	Field::Open(Group::Hotspot),
	Field::Save,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleFocus {
	Target,
	Message,
}

/// The home menu, without screens for services left out of the build.
const MENU: &[(&str, Screen)] = &[
	("Network", Screen::Network),
	("Radio", Screen::Radio),
	("Ping", Screen::Ping),
	#[cfg(feature = "console")]
	("Console", Screen::Console),
	#[cfg(feature = "irc")]
	("IRC", Screen::Irc),
	#[cfg(feature = "gemini")]
	("Gemini", Screen::Gemini),
	#[cfg(feature = "gopher")]
	("Gopher", Screen::Gopher),
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
	completion: Option<Completion>,

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

	gopher: gopher::Browser,
	irc: irc::Client,
	#[cfg(feature = "gemini")]
	gemini: gemini::Browser,

	saved: Settings,
	draft: Settings,
	settings_selected: usize,
	group_selected: usize,
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
			completion: None,
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
			gopher: gopher::Browser::new(),
			irc: irc::Client::new(&default_nick(&settings.name)),
			#[cfg(feature = "gemini")]
			gemini: gemini::Browser::new(),
			draft: settings.clone(),
			saved: settings,
			settings_selected: 0,
			group_selected: 0,
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
		// Names are looked up where they are shown or can be typed.
		if matches!(
			self.screen,
			Screen::Network | Screen::Node(_) | Screen::Radio
		) || self.has_address_field()
		{
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
			Event::Fetched { data } => self.gopher.fetched(&data),
			Event::FetchFailed => self.gopher.failed(),
			Event::StreamOpened(Stream::Irc) => {
				let mut commands = Vec::new();
				self.irc.opened(&mut commands);
				self.commands.extend(commands);
			}
			Event::StreamData {
				stream: Stream::Irc,
				data,
			} => {
				let mut commands = Vec::new();
				let viewing = self.screen == Screen::Irc;
				self.irc.received(&data, viewing, &mut commands);
				self.commands.extend(commands);
			}
			Event::StreamClosed(Stream::Irc) => self.irc.closed(),
			#[cfg(feature = "gemini")]
			Event::StreamOpened(Stream::Gemini) => {
				let mut commands = Vec::new();
				self.gemini.opened(&mut commands);
				self.commands.extend(commands);
			}
			#[cfg(feature = "gemini")]
			Event::StreamData {
				stream: Stream::Gemini,
				data,
			} => {
				let mut commands = Vec::new();
				self.gemini.received(&data, &mut commands);
				self.commands.extend(commands);
			}
			#[cfg(feature = "gemini")]
			Event::StreamClosed(Stream::Gemini) => {
				let mut commands = Vec::new();
				self.gemini.closed(&mut commands);
				self.commands.extend(commands);
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
		if key != Key::Tab {
			self.completion = None;
		} else if self.complete_address() {
			return;
		}
		match self.screen {
			Screen::Home => self.home_key(key),
			Screen::Network => self.network_key(key),
			Screen::Node(node) => self.node_key(node, key),
			Screen::Radio => {
				if matches!(navigation(key), Some(Key::Escape | Key::Left)) {
					self.screen = Screen::Home;
				}
			}
			Screen::Ping => self.ping_key(key),
			Screen::Console => self.console_key(key),
			Screen::Settings | Screen::SettingsGroup(_) => self.settings_key(key),
			Screen::Networks => self.networks_key(key),
			Screen::Irc => {
				let mut commands = Vec::new();
				let outcome = self.irc.handle_key(key, &mut commands);
				self.commands.extend(commands);
				if let irc::Outcome::Leave = outcome {
					self.screen = Screen::Home;
				}
			}
			Screen::Gemini => {
				#[cfg(feature = "gemini")]
				{
					let mut commands = Vec::new();
					let outcome = self.gemini.handle_key(key, &mut commands);
					self.commands.extend(commands);
					if let gemini::Outcome::Leave = outcome {
						self.screen = Screen::Home;
					}
				}
			}
			Screen::Gopher => {
				let mut commands = Vec::new();
				let outcome = self.gopher.handle_key(key, &mut commands);
				self.commands.extend(commands);
				if let gopher::Outcome::Leave = outcome {
					self.screen = Screen::Home;
				}
			}
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
		if screen == Screen::Irc {
			self.irc.mark_read();
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
		let (fields, selected) = self.settings_fields();
		match navigation(key) {
			Some(Key::Up) => *selected = selected.saturating_sub(1),
			Some(Key::Down) => *selected = (*selected + 1).min(fields.len() - 1),
			Some(Key::Escape | Key::Left) => {
				self.screen = match self.screen {
					Screen::SettingsGroup(_) => Screen::Settings,
					_ => Screen::Home,
				}
			}
			Some(Key::Enter | Key::Right) => {
				let field = fields[*selected];
				self.activate(field);
			}
			_ => {}
		}
	}

	/// The settings on the screen shown, and which of them is selected.
	fn settings_fields(&mut self) -> (&'static [Field], &mut usize) {
		match self.screen {
			Screen::SettingsGroup(group) => (group.fields(), &mut self.group_selected),
			_ => (FIELDS, &mut self.settings_selected),
		}
	}

	fn activate(&mut self, field: Field) {
		let input = match field {
			Field::Name => TextInput::prefilled(&self.draft.name, 32),
			Field::Password => TextInput::secret(&self.draft.password, 63),
			Field::Peer => TextInput::prefilled(&self.draft.peer, 21),
			Field::Channel => TextInput::prefilled(&self.draft.channel.to_string(), 2),
			Field::TimeZone => TextInput::prefilled(&format_offset(self.draft.utc_offset), 6),
			Field::HotspotName => TextInput::prefilled(&self.draft.hotspot_name, 32),
			Field::HotspotPassword => TextInput::secret(&self.draft.hotspot_password, 63),
			Field::Bridge => {
				self.draft.bridge = !self.draft.bridge;
				return;
			}
			Field::Hotspot => {
				self.draft.hotspot = !self.draft.hotspot;
				return;
			}
			Field::StatusPage => {
				self.draft.status_page = !self.draft.status_page;
				return;
			}
			Field::Open(group) => {
				self.group_selected = 0;
				self.screen = Screen::SettingsGroup(group);
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
			Field::TimeZone => match parse_offset(&text) {
				Some(offset) => self.draft.utc_offset = offset,
				None => self.notice = Some("Use an offset from UTC, like -05:00".into()),
			},
			Field::Channel => match text.parse::<u8>() {
				Ok(channel @ 1..=13) => self.draft.channel = channel,
				_ => self.notice = Some("Channels go from 1 to 13".into()),
			},
			Field::HotspotName if text.is_empty() => {
				self.notice = Some("The hotspot needs a name".into())
			}
			Field::HotspotName => self.draft.hotspot_name = text,
			Field::HotspotPassword if Settings::is_hotspot_password(&text) => {
				self.draft.hotspot_password = text
			}
			Field::HotspotPassword => {
				self.notice = Some("Use 8 or more characters, or none".into())
			}
			Field::Bridge
			| Field::StatusPage
			| Field::Network
			| Field::Hotspot
			| Field::Open(_)
			| Field::Save => {}
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
					self.screen = Screen::SettingsGroup(Group::Bridge);
				}
			}
			Some(Key::Escape | Key::Left) => self.screen = Screen::SettingsGroup(Group::Bridge),
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

	fn has_address_field(&self) -> bool {
		matches!(
			self.screen,
			Screen::Ping | Screen::Console | Screen::Gopher | Screen::Gemini | Screen::Irc
		)
	}

	/// The field being typed in, if it takes a node's address.
	fn address_field(&mut self) -> Option<&mut TextInput> {
		match self.screen {
			Screen::Ping if !self.pinging => Some(&mut self.ping_target),
			Screen::Console if self.console_focus == ConsoleFocus::Target => {
				Some(&mut self.console_target)
			}
			Screen::Gopher if self.gopher.editing && self.gopher.search.is_none() => {
				Some(&mut self.gopher.address)
			}
			#[cfg(feature = "gemini")]
			Screen::Gemini if self.gemini.editing && self.gemini.prompt.is_none() => {
				Some(&mut self.gemini.address)
			}
			Screen::Irc if self.irc.phase == irc::Phase::Setup && self.irc.field == 0 => {
				Some(&mut self.irc.form[0])
			}
			_ => None,
		}
	}

	/// Fills in the address of a node whose name or address starts with what
	/// was typed, or the next such node on another Tab. Returns whether
	/// there was anything to complete, so Tab can do its usual job if not.
	fn complete_address(&mut self) -> bool {
		let Some(typed) = self.address_field().map(|field| field.text.clone()) else {
			return false;
		};
		let cycling = self.completion.as_ref().is_some_and(|completion| {
			completion.matches.len() > 1
				&& completion.matches[completion.index].0.to_string() == typed
		});
		if cycling {
			let completion = self.completion.as_mut().expect("checked above");
			completion.index = (completion.index + 1) % completion.matches.len();
		} else {
			let matches = self.nodes_starting_with(&typed);
			let done = matches.len() == 1 && matches[0].0.to_string() == typed;
			if matches.is_empty() || done {
				return false;
			}
			self.completion = Some(Completion { matches, index: 0 });
		}
		let Some(completion) = &self.completion else {
			return false;
		};
		let (address, name) = &completion.matches[completion.index];
		let count = completion.matches.len();
		let position = completion.index + 1;
		self.notice = match (name, count) {
			(Some(name), 1) => Some(name.clone()),
			(Some(name), _) => Some(format!("{name}  {position} of {count}")),
			(None, 1) => None,
			(None, _) => Some(format!("{position} of {count}")),
		};
		let address = address.to_string();
		if let Some(field) = self.address_field() {
			field.set(&address);
		}
		true
	}

	fn nodes_starting_with(&self, typed: &str) -> Vec<(Ipv6Addr, Option<String>)> {
		let typed = typed.trim().trim_start_matches('[').to_ascii_lowercase();
		if typed.is_empty() {
			return Vec::new();
		}
		self.nodes()
			.iter()
			.map(|key| {
				let address = Ipv6Addr::from(addr_for_key(key).0);
				(address, self.name_of(key).map(String::from))
			})
			.filter(|(address, name)| {
				address.to_string().starts_with(&typed)
					|| name
						.as_ref()
						.is_some_and(|name| name.to_ascii_lowercase().starts_with(&typed))
			})
			.collect()
	}

	fn start_lookups(&mut self) {
		let pending = self
			.lookups
			.values()
			.filter(|lookup| matches!(lookup, Lookup::Pending { .. }))
			.count();
		let wanted: Vec<PublicKey> = match self.screen {
			Screen::Node(node) => Vec::from([node]),
			Screen::Radio => self
				.status
				.iter()
				.flat_map(|status| &status.neighbors)
				.filter_map(|neighbor| neighbor.key)
				.collect(),
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

/// A nick from the board's name, keeping only what IRC allows.
fn default_nick(name: &str) -> String {
	let nick: String = name
		.chars()
		.filter(|character| character.is_ascii_alphanumeric() || "-_[]{}\\|^`".contains(*character))
		.take(16)
		.collect();
	if nick.starts_with(|character: char| character.is_ascii_alphabetic()) {
		nick
	} else {
		String::from("esp-ygg")
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
