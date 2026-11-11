//! The on-device user interface. [`App`] holds the state and turns key
//! presses into [`Command`]s for the host to carry out; [`draw`] renders it
//! with Ratatui. Another renderer could replace [`draw`] without touching
//! the rest.

#[cfg(feature = "voice")]
mod call;
#[cfg(feature = "console")]
mod console;
#[cfg(feature = "gemini")]
mod gemini;
#[cfg(feature = "gopher")]
mod gopher;
mod input;
#[cfg(feature = "irc")]
mod irc;
pub mod keyboard;
mod ping;
pub mod remote;
mod view;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;
#[cfg(any(
	feature = "console",
	feature = "gopher",
	feature = "irc",
	feature = "gemini"
))]
use core::net::SocketAddrV6;

use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Instant, Mac, Node, Platform, PublicKey};

use crate::BoardInfo;
use crate::overlay::Event;
#[cfg(any(feature = "irc", feature = "gemini"))]
use crate::overlay::Stream;
use crate::settings::Settings;
use crate::time::{format_offset, parse_offset};
pub use input::TextInput;
pub use view::draw;

const LOG_LINES: usize = 64;
/// Nodeinfo requests in flight at once, so lookups do not crowd the radio.
const MAX_LOOKUPS: usize = 3;
/// Lookups that time out are retried this many times.
const LOOKUP_ATTEMPTS: u8 = 3;

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
/// What the host has been asked to do, oldest first.
type Commands = VecDeque<Command>;

/// Whether a key keeps an app's screen open.
enum Outcome {
	Stay,
	Leave,
	/// Stays, with a word about what went wrong.
	Notice(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
	Ping(Ipv6Addr),
	#[cfg(feature = "console")]
	SendDatagram(SocketAddrV6, Vec<u8>),
	RequestNodeInfo(PublicKey),
	/// Send a request to a TCP server and collect its whole response.
	#[cfg(feature = "gopher")]
	Fetch(SocketAddrV6, Vec<u8>),
	/// Open a lasting TCP connection, send on it, and close it.
	#[cfg(any(feature = "irc", feature = "gemini"))]
	OpenStream(Stream, SocketAddrV6),
	#[cfg(any(feature = "irc", feature = "gemini"))]
	StreamSend(Stream, Vec<u8>),
	#[cfg(any(feature = "irc", feature = "gemini"))]
	CloseStream(Stream),
	ScanWifi,
	/// Store these settings and restart to apply them.
	Save(Settings),
	#[cfg(feature = "voice")]
	Call(Ipv6Addr),
	#[cfg(feature = "voice")]
	Answer,
	/// Hang up, or decline a call.
	#[cfg(feature = "voice")]
	HangUp,
	/// Play the start of a ringtone, so the user hears what they picked.
	#[cfg(feature = "voice")]
	PreviewRinger {
		tone: u8,
		volume: u8,
		samples: usize,
	},
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
	#[cfg(feature = "apps")]
	Apps,
	Nodes,
	Node(PublicKey),
	Neighbors,
	Ping,
	#[cfg(feature = "console")]
	Console,
	#[cfg(feature = "gopher")]
	Gopher,
	#[cfg(feature = "irc")]
	Irc,
	#[cfg(feature = "gemini")]
	Gemini,
	#[cfg(feature = "voice")]
	Call,
	Settings,
	/// The settings in one group, opened from the main settings.
	SettingsGroup(Group),
	WifiNetworks,
}

/// Settings that open as a menu of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
	Bridge,
	Hotspot,
	#[cfg(feature = "voice")]
	Ringer,
}

impl Group {
	fn fields(self) -> &'static [Field] {
		match self {
			Group::Bridge => &[Field::Bridge, Field::Ssid, Field::WifiPassword, Field::Peer],
			Group::Hotspot => &[Field::Hotspot, Field::HotspotName, Field::HotspotPassword],
			#[cfg(feature = "voice")]
			Group::Ringer => &[Field::Ringtone, Field::RingVolume],
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
	Ssid,
	WifiPassword,
	Peer,
	Hotspot,
	HotspotName,
	HotspotPassword,
	#[cfg(feature = "voice")]
	Ringtone,
	#[cfg(feature = "voice")]
	RingVolume,
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
	#[cfg(feature = "voice")]
	Field::Open(Group::Ringer),
	Field::Save,
];

/// The home menu, without screens for services left out of the build.
const MENU: &[(&str, Screen)] = &[
	("Yggdrasil Nodes", Screen::Nodes),
	("ESP-NOW Neighbors", Screen::Neighbors),
	("Ping", Screen::Ping),
	#[cfg(feature = "apps")]
	("Apps", Screen::Apps),
	("Settings", Screen::Settings),
];

/// Menu items that open a menu of their own.
const MENUS: &[Screen] = &[
	#[cfg(feature = "apps")]
	Screen::Apps,
];

/// The apps menu, opened from the home menu.
#[cfg(feature = "apps")]
const APPS: &[(&str, Screen)] = &[
	#[cfg(feature = "console")]
	("UDP Console", Screen::Console),
	#[cfg(feature = "voice")]
	("Voice Call (IAX2)", Screen::Call),
	#[cfg(feature = "irc")]
	("IRC", Screen::Irc),
	#[cfg(feature = "gemini")]
	("Gemini", Screen::Gemini),
	#[cfg(feature = "gopher")]
	("Gopher", Screen::Gopher),
];

pub struct App {
	now: Instant,
	status: Option<Status>,
	screen: Screen,
	menu: usize,
	#[cfg(feature = "apps")]
	apps_selected: usize,
	commands: VecDeque<Command>,
	/// A short message about the last action, cleared by the next key.
	notice: Option<String>,
	completion: Option<Completion>,

	lookups: BTreeMap<PublicKey, Lookup>,
	nodes_selected: usize,

	ping: ping::Ping,
	#[cfg(feature = "console")]
	console: console::Console,

	#[cfg(feature = "gopher")]
	gopher: gopher::Browser,
	#[cfg(feature = "irc")]
	irc: irc::Client,
	#[cfg(feature = "gemini")]
	gemini: gemini::Browser,
	#[cfg(feature = "voice")]
	call: call::Phone,

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
			#[cfg(feature = "apps")]
			apps_selected: 0,
			commands: VecDeque::new(),
			notice: None,
			completion: None,
			lookups: BTreeMap::new(),
			nodes_selected: 0,
			ping: ping::Ping::new(),
			#[cfg(feature = "console")]
			console: console::Console::new(),
			#[cfg(feature = "gopher")]
			gopher: gopher::Browser::new(),
			#[cfg(feature = "irc")]
			irc: irc::Client::new(&default_nick(&settings.name)),
			#[cfg(feature = "gemini")]
			gemini: gemini::Browser::new(),
			#[cfg(feature = "voice")]
			call: call::Phone::new(),
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
		self.ping.tick(now, &mut self.commands);
		// Names are looked up where they are shown or can be typed.
		if matches!(
			self.screen,
			Screen::Nodes | Screen::Node(_) | Screen::Neighbors
		) || self.has_address_field()
		{
			self.start_lookups();
		}
	}

	pub fn handle_event(&mut self, event: Event) {
		match event {
			#[cfg(feature = "voice")]
			Event::Call(event) => {
				if self.call.handle_event(event, self.now) {
					self.screen = Screen::Call;
				}
			}
			Event::PingReply {
				from,
				sequence,
				round_trip,
			} => self.ping.replied(from, sequence, round_trip),
			Event::PingTimeout { target, sequence } => self.ping.timed_out(target, sequence),
			#[cfg(feature = "gopher")]
			Event::Fetched { data } => self.gopher.fetched(&data),
			#[cfg(feature = "gopher")]
			Event::FetchFailed => self.gopher.failed(),
			#[cfg(feature = "irc")]
			Event::StreamOpened(Stream::Irc) => {
				self.irc.opened(&mut self.commands);
			}
			#[cfg(feature = "irc")]
			Event::StreamData {
				stream: Stream::Irc,
				data,
			} => {
				let viewing = self.screen == Screen::Irc;
				self.irc.received(&data, viewing, &mut self.commands);
			}
			#[cfg(feature = "irc")]
			Event::StreamClosed(Stream::Irc) => self.irc.closed(),
			#[cfg(feature = "gemini")]
			Event::StreamOpened(Stream::Gemini) => {
				self.gemini.opened(&mut self.commands);
			}
			#[cfg(feature = "gemini")]
			Event::StreamData {
				stream: Stream::Gemini,
				data,
			} => {
				self.gemini.received(&data, &mut self.commands);
			}
			#[cfg(feature = "gemini")]
			Event::StreamClosed(Stream::Gemini) => {
				self.gemini.closed(&mut self.commands);
			}
			#[cfg(feature = "console")]
			Event::Datagram { from, data } => {
				let sender = self.short_name_of_address(from.ip());
				let viewing = self.screen == Screen::Console;
				self.console.received(&sender, &data, viewing);
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
		let outcome = match self.screen {
			Screen::Home => self.home_key(key),
			#[cfg(feature = "apps")]
			Screen::Apps => self.apps_key(key),
			Screen::Nodes => self.nodes_key(key),
			Screen::Node(node) => self.node_key(node, key),
			Screen::Neighbors => match navigation(key) {
				Some(Key::Escape | Key::Left) => Outcome::Leave,
				_ => Outcome::Stay,
			},
			Screen::Settings | Screen::SettingsGroup(_) => self.settings_key(key),
			Screen::WifiNetworks => self.wifi_networks_key(key),
			Screen::Ping => self.ping.handle_key(key),
			#[cfg(feature = "console")]
			Screen::Console => self.console.handle_key(key, &mut self.commands),
			#[cfg(feature = "irc")]
			Screen::Irc => self.irc.handle_key(key, &mut self.commands),
			#[cfg(feature = "gopher")]
			Screen::Gopher => self.gopher.handle_key(key, &mut self.commands),
			#[cfg(feature = "gemini")]
			Screen::Gemini => self.gemini.handle_key(key, &mut self.commands),
			#[cfg(feature = "voice")]
			Screen::Call => self.call.handle_key(key, self.now, &mut self.commands),
		};
		match outcome {
			Outcome::Stay => {}
			Outcome::Leave => self.screen = self.parent(),
			Outcome::Notice(notice) => self.notice = Some(notice.into()),
		}
	}

	/// Where leaving the current screen goes.
	fn parent(&self) -> Screen {
		match self.screen {
			Screen::Node(_) => Screen::Nodes,
			Screen::SettingsGroup(_) => Screen::Settings,
			Screen::WifiNetworks => Screen::SettingsGroup(Group::Bridge),
			#[cfg(feature = "apps")]
			screen if APPS.iter().any(|&(_, app)| app == screen) => Screen::Apps,
			_ => Screen::Home,
		}
	}

	fn home_key(&mut self, key: Key) -> Outcome {
		match navigation(key) {
			Some(Key::Up) => self.menu = self.menu.saturating_sub(1),
			Some(Key::Down) => self.menu = (self.menu + 1).min(MENU.len() - 1),
			Some(Key::Enter | Key::Right) => self.open(MENU[self.menu].1),
			_ => {}
		}
		Outcome::Stay
	}

	#[cfg(feature = "apps")]
	fn apps_key(&mut self, key: Key) -> Outcome {
		match navigation(key) {
			Some(Key::Up) => self.apps_selected = self.apps_selected.saturating_sub(1),
			Some(Key::Down) => {
				self.apps_selected = (self.apps_selected + 1).min(APPS.len().saturating_sub(1))
			}
			Some(Key::Enter | Key::Right) => {
				if let Some(&(_, screen)) = APPS.get(self.apps_selected) {
					self.open(screen);
				}
			}
			Some(Key::Escape | Key::Left) => return Outcome::Leave,
			_ => {}
		}
		Outcome::Stay
	}

	fn open(&mut self, screen: Screen) {
		match screen {
			#[cfg(feature = "console")]
			Screen::Console => self.console.unread = 0,
			#[cfg(feature = "irc")]
			Screen::Irc => self.irc.mark_read(),
			Screen::Settings => {
				self.draft = self.saved.clone();
				self.settings_selected = 0;
			}
			_ => {}
		}
		self.screen = screen;
	}

	fn nodes_key(&mut self, key: Key) -> Outcome {
		let count = self.nodes().len();
		match navigation(key) {
			Some(Key::Up) => self.nodes_selected = self.nodes_selected.saturating_sub(1),
			Some(Key::Down) => {
				self.nodes_selected = (self.nodes_selected + 1).min(count.saturating_sub(1))
			}
			Some(Key::Enter | Key::Right) => {
				if let Some(&node) = self.nodes().get(self.nodes_selected) {
					self.screen = Screen::Node(node);
				}
			}
			Some(Key::Escape | Key::Left) => return Outcome::Leave,
			_ => {}
		}
		Outcome::Stay
	}

	fn node_key(&mut self, node: PublicKey, key: Key) -> Outcome {
		let address = Ipv6Addr::from(addr_for_key(&node).0);
		match key {
			Key::Char('p') => {
				self.ping.start_at(address);
				self.screen = Screen::Ping;
			}
			#[cfg(feature = "console")]
			Key::Char('m') => {
				self.console.write_to(address);
				self.open(Screen::Console);
			}
			_ if matches!(navigation(key), Some(Key::Escape | Key::Left)) => return Outcome::Leave,
			_ => {}
		}
		Outcome::Stay
	}

	fn settings_key(&mut self, key: Key) -> Outcome {
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
			return Outcome::Stay;
		}
		let (fields, selected) = self.settings_fields();
		match navigation(key) {
			Some(Key::Up) => *selected = selected.saturating_sub(1),
			Some(Key::Down) => *selected = (*selected + 1).min(fields.len() - 1),
			Some(Key::Escape | Key::Left) => return Outcome::Leave,
			Some(Key::Enter | Key::Right) => {
				let field = fields[*selected];
				self.activate(field);
			}
			_ => {}
		}
		Outcome::Stay
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
			Field::WifiPassword => TextInput::secret(&self.draft.password, 63),
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
			#[cfg(feature = "voice")]
			Field::Ringtone => {
				let count = crate::voice::ringtone::RINGTONES.len() as u8;
				self.draft.ringtone = (self.draft.ringtone + 1) % count;
				self.commands.push_back(Command::PreviewRinger {
					tone: self.draft.ringtone,
					volume: self.draft.ring_volume,
					samples: crate::voice::ringtone::PREVIEW_SAMPLES,
				});
				return;
			}
			#[cfg(feature = "voice")]
			Field::RingVolume => {
				let levels = crate::voice::ringtone::LOUDEST + 1;
				self.draft.ring_volume = (self.draft.ring_volume + 1) % levels;
				// A single beep shows how loud it is.
				self.commands.push_back(Command::PreviewRinger {
					tone: crate::voice::ringtone::BEEPS,
					volume: self.draft.ring_volume,
					samples: crate::voice::ringtone::BEEP_SAMPLES,
				});
				return;
			}
			Field::Open(group) => {
				self.group_selected = 0;
				self.screen = Screen::SettingsGroup(group);
				return;
			}
			Field::Ssid => {
				self.networks = None;
				self.commands.push_back(Command::ScanWifi);
				self.screen = Screen::WifiNetworks;
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
			Field::WifiPassword => self.draft.password = text,
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
			| Field::Ssid
			| Field::Hotspot
			| Field::Open(_)
			| Field::Save => {}
			#[cfg(feature = "voice")]
			Field::Ringtone | Field::RingVolume => {}
		}
	}

	fn wifi_networks_key(&mut self, key: Key) -> Outcome {
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
			Some(Key::Escape | Key::Left) => return Outcome::Leave,
			Some(Key::Char('r')) => {
				self.networks = None;
				self.commands.push_back(Command::ScanWifi);
			}
			_ => {}
		}
		Outcome::Stay
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
		match self.screen {
			Screen::Ping => true,
			#[cfg(feature = "console")]
			Screen::Console => true,
			#[cfg(feature = "gopher")]
			Screen::Gopher => true,
			#[cfg(feature = "gemini")]
			Screen::Gemini => true,
			#[cfg(feature = "irc")]
			Screen::Irc => true,
			#[cfg(feature = "voice")]
			Screen::Call => true,
			_ => false,
		}
	}

	/// The field being typed in, if it takes a node's address.
	fn address_field(&mut self) -> Option<&mut TextInput> {
		match self.screen {
			Screen::Ping if !self.ping.running => Some(&mut self.ping.target),
			#[cfg(feature = "console")]
			Screen::Console if self.console.focus == console::Focus::Target => {
				Some(&mut self.console.target)
			}
			#[cfg(feature = "gopher")]
			Screen::Gopher if self.gopher.editing && self.gopher.search.is_none() => {
				Some(&mut self.gopher.address)
			}
			#[cfg(feature = "gemini")]
			Screen::Gemini if self.gemini.editing && self.gemini.prompt.is_none() => {
				Some(&mut self.gemini.address)
			}
			#[cfg(feature = "voice")]
			Screen::Call if self.call.phase == call::Phase::Idle => Some(&mut self.call.address),
			#[cfg(feature = "irc")]
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
			Screen::Neighbors => self
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

	/// Shows whether push-to-talk is held.
	#[cfg(feature = "voice")]
	pub fn set_talking(&mut self, talking: bool) {
		self.call.talking = talking;
	}

	/// A node's name if known, or its address.
	#[cfg(any(feature = "console", feature = "voice"))]
	fn short_name_of_address(&self, address: &Ipv6Addr) -> String {
		self.lookups
			.iter()
			.find(|(key, _)| Ipv6Addr::from(addr_for_key(key).0) == *address)
			.and_then(|(key, _)| self.name_of(key))
			.map_or_else(|| address.to_string(), String::from)
	}
}

/// A nick from the board's name, keeping only what IRC allows.
#[cfg(feature = "irc")]
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
