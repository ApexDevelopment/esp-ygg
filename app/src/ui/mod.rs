//! The on-device user interface. [`App`] holds the state and turns key
//! presses into [`Command`]s for the host to carry out; [`draw`] renders it
//! with Ratatui. Another renderer could replace [`draw`] without touching
//! the rest.

#[cfg(feature = "voice")]
mod call;
#[cfg(feature = "console")]
pub mod console;
pub mod directory;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "gopher")]
pub mod gopher;
mod input;
#[cfg(feature = "irc")]
pub mod irc;
pub mod keyboard;
mod ping;
pub mod remote;
mod screen_keyboard;
pub mod screen_timeout;
pub mod touch;
mod view;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;
use core::net::Ipv6Addr;
#[cfg(any(
	feature = "console",
	feature = "gopher",
	feature = "irc",
	feature = "gemini"
))]
use core::net::SocketAddrV6;

use ratatui::layout::{Position, Rect};
use screen_keyboard::{Kind, ScreenKeyboard, Tapped};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Instant, Mac, Node, Platform, PublicKey};

use crate::BoardInfo;
use crate::overlay::Event;
#[cfg(any(feature = "irc", feature = "gemini"))]
use crate::overlay::Stream;
use crate::settings::{Settings, TextSetting};
use crate::time::DateTime;
use directory::Directory;
pub use input::TextInput;
#[cfg(feature = "voice")]
pub use view::SPEAKER_FROM_RIGHT;
pub use view::draw;

const LOG_LINES: usize = 64;

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
pub type Commands = VecDeque<Command>;

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
	/// Write settings, as text, to the SD card. Answer with
	/// [`App::settings_exported`].
	ExportSettings(String),
	/// Read settings from the SD card. Answer with [`App::settings_imported`].
	ImportSettings,
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

/// The speaker icon in the title bar, which the host draws.
#[cfg(feature = "voice")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
	On,
	/// The ringer volume is off.
	Silent,
	/// Muted over the serial port, for testing.
	Muted,
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
	/// Known nodes, direct peers first, then by address.
	pub fn nodes(&self) -> Vec<PublicKey> {
		let mut nodes = self.known.clone();
		for peer in &self.peers {
			if !nodes.contains(peer) && *peer != self.public_key {
				nodes.push(*peer);
			}
		}
		nodes.sort_by_key(|key| (!self.peers.contains(key), addr_for_key(key).0));
		nodes
	}

	/// ESP-NOW neighbors that have finished the Yggdrasil handshake.
	pub fn connected_neighbors(&self) -> usize {
		self.neighbors
			.iter()
			.filter(|neighbor| neighbor.key.is_some())
			.count()
	}

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

impl Neighbor {
	/// How strong its signal is, from none to four bars, once heard.
	pub fn signal_bars(&self) -> Option<usize> {
		Some(match self.signal? {
			-55.. => 4,
			-65.. => 3,
			-75.. => 2,
			-85.. => 1,
			_ => 0,
		})
	}

	/// Its MAC address, as it is usually written.
	pub fn mac_text(&self) -> String {
		let [a, b, c, d, e, f] = self.mac;
		format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
	pub ssid: String,
	/// In dBm.
	pub signal: i8,
	pub secured: bool,
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
	Export,
	Import,
}

impl Field {
	/// The setting the field is typed into, if it is one.
	fn text_setting(self) -> Option<TextSetting> {
		Some(match self {
			Field::Name => TextSetting::Name,
			Field::Channel => TextSetting::Channel,
			Field::TimeZone => TextSetting::TimeZone,
			Field::WifiPassword => TextSetting::WifiPassword,
			Field::Peer => TextSetting::Peer,
			Field::HotspotName => TextSetting::HotspotName,
			Field::HotspotPassword => TextSetting::HotspotPassword,
			_ => return None,
		})
	}
}

/// Where settings are exported to and imported from on the SD card.
pub const SETTINGS_FILE: &str = "esp-ygg/settings.txt";

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
	Field::Export,
	Field::Import,
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
	/// Locked against input, showing only that it is.
	locked: bool,
	completion: Option<Completion>,

	directory: Directory,
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

	keyboard: ScreenKeyboard,
	/// The whole screen, as last drawn, for placing the keyboard.
	drawn: Cell<Rect>,
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
			locked: false,
			completion: None,
			directory: Directory::new(),
			nodes_selected: 0,
			ping: ping::Ping::new(),
			#[cfg(feature = "console")]
			console: console::Console::new(),
			#[cfg(feature = "gopher")]
			gopher: gopher::Browser::new(),
			#[cfg(feature = "irc")]
			irc: irc::Client::new(&irc::default_nick(&settings.name)),
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
			keyboard: ScreenKeyboard::default(),
			drawn: Cell::new(Rect::default()),
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
		self.keyboard.tick(now);
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
		if self.directory.handle_nodeinfo(key, info.as_deref()) {
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
	}

	/// Locks against input, or unlocks.
	pub fn set_locked(&mut self, locked: bool) {
		if locked {
			self.keyboard.release();
		}
		self.locked = locked;
	}

	pub fn is_locked(&self) -> bool {
		self.locked
	}

	pub fn handle_key(&mut self, key: Key) {
		if self.locked {
			return;
		}
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
		if self.typing().is_none() {
			self.keyboard.hide();
		}
	}

	/// A tap on a touch screen, at a column and row of the screen. Taps on
	/// the keyboard type. Any other tap brings the keyboard up while there
	/// is a text field to type in, and presses Enter otherwise.
	pub fn handle_tap(&mut self, column: u16, row: u16) {
		if self.locked {
			return;
		}
		let point = Position::new(column, row);
		if let Some((area, kind)) = self.keyboard_area()
			&& area.contains(point)
		{
			match self.keyboard.tap(area, kind, point) {
				Some(Tapped::Key(key)) => self.handle_key(key),
				Some(Tapped::Hide) => self.keyboard.hide(),
				None => {}
			}
			return;
		}
		if self.typing().is_none() {
			self.handle_key(Key::Enter);
		}
		if self.typing().is_some() {
			self.keyboard.show();
		}
	}

	/// A finger has touched the screen at a column and row. Keys on the
	/// keyboard light up under it.
	pub fn handle_press(&mut self, column: u16, row: u16) {
		if self.locked {
			return;
		}
		if let Some((area, kind)) = self.keyboard_area() {
			self.keyboard.press(area, kind, Position::new(column, row));
		}
	}

	/// The finger has lifted off the screen.
	pub fn handle_release(&mut self) {
		self.keyboard.release();
	}

	/// What kind of text field keys go into, if any.
	fn typing(&self) -> Option<Kind> {
		let text = match self.screen {
			Screen::Settings | Screen::SettingsGroup(_) => {
				return self.editing.as_ref().map(|(field, _)| match field {
					Field::Channel | Field::TimeZone | Field::Peer => Kind::Number,
					_ => Kind::Text,
				});
			}
			Screen::Ping => !self.ping.running,
			#[cfg(feature = "console")]
			Screen::Console => true,
			#[cfg(feature = "gopher")]
			Screen::Gopher => self.gopher.editing || self.gopher.search.is_some(),
			#[cfg(feature = "gemini")]
			Screen::Gemini => self.gemini.editing || self.gemini.prompt.is_some(),
			#[cfg(feature = "irc")]
			Screen::Irc => true,
			#[cfg(feature = "voice")]
			Screen::Call => self.call.phase == call::Phase::Idle,
			_ => false,
		};
		text.then_some(Kind::Text)
	}

	/// Where the on-screen keyboard is, and for what kind of field, while
	/// it is up.
	fn keyboard_area(&self) -> Option<(Rect, Kind)> {
		let kind = self.typing().filter(|_| self.keyboard.is_shown())?;
		let screen = self.drawn.get();
		let height = ScreenKeyboard::height(kind).min(screen.height);
		Some((view::split_bottom(screen, height).1, kind))
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
		if let Some(setting) = field.text_setting() {
			let text = self.draft.text(setting);
			let input = if setting.is_secret() {
				TextInput::secret(&text, setting.max_len())
			} else {
				TextInput::prefilled(&text, setting.max_len())
			};
			self.editing = Some((field, input));
			return;
		}
		match field {
			Field::Bridge => {
				self.draft.bridge = !self.draft.bridge;
			}
			Field::Hotspot => {
				self.draft.hotspot = !self.draft.hotspot;
			}
			Field::StatusPage => {
				self.draft.status_page = !self.draft.status_page;
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
			}
			Field::Open(group) => {
				self.group_selected = 0;
				self.screen = Screen::SettingsGroup(group);
			}
			Field::Ssid => {
				self.networks = None;
				self.commands.push_back(Command::ScanWifi);
				self.screen = Screen::WifiNetworks;
			}
			Field::Save => {
				if self.draft.bridge && !self.draft.can_bridge() {
					self.notice = Some("Bridge needs network and peer".into());
					return;
				}
				self.saved = self.draft.clone();
				self.commands.push_back(Command::Save(self.draft.clone()));
				self.notice = Some("Restarting".into());
			}
			Field::Export => {
				let text = self.draft.to_text();
				self.commands.push_back(Command::ExportSettings(text));
			}
			Field::Import => {
				self.commands.push_back(Command::ImportSettings);
			}
			// Fields typed in were opened above.
			_ => {}
		}
	}

	fn apply_field(&mut self, field: Field, text: String) {
		if let Some(setting) = field.text_setting()
			&& let Err(problem) = self.draft.set_text(setting, &text)
		{
			self.notice = Some(problem.into());
		}
	}

	/// How writing settings to the SD card went.
	pub fn settings_exported(&mut self, result: Result<(), String>) {
		self.notice = Some(match result {
			Ok(()) => "Exported".into(),
			Err(problem) => problem,
		});
	}

	/// The settings file read from the SD card, or why it could not be.
	/// What it holds replaces the settings on screen, until they are saved.
	pub fn settings_imported(&mut self, result: Result<String, String>) {
		let imported = result.and_then(|text| self.draft.with_text(&text));
		self.notice = Some(match imported {
			Ok(settings) => {
				self.draft = settings;
				"Imported, not saved".into()
			}
			Err(problem) => problem,
		});
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

	fn nodes(&self) -> Vec<PublicKey> {
		self.status.as_ref().map(Status::nodes).unwrap_or_default()
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
		for key in self.directory.start_lookups(wanted) {
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
	}

	fn name_of(&self, key: &PublicKey) -> Option<&str> {
		self.directory.name_of(key)
	}

	/// The local date and time, once the board knows it. While settings are
	/// open, it follows the time zone being chosen.
	pub fn local_time(&self) -> Option<DateTime> {
		let time = self.status.as_ref()?.board.time?;
		let offset = if matches!(self.screen, Screen::Settings | Screen::SettingsGroup(_)) {
			self.draft.utc_offset
		} else {
			self.saved.utc_offset
		};
		Some(DateTime::from_unix((time / 1_000_000) as i64, offset))
	}

	/// The speaker icon for the title bar. While settings are open, it
	/// follows the ringer volume being chosen.
	#[cfg(feature = "voice")]
	pub fn speaker(&self, muted: bool) -> Speaker {
		let settings = if matches!(self.screen, Screen::Settings | Screen::SettingsGroup(_)) {
			&self.draft
		} else {
			&self.saved
		};
		if muted {
			Speaker::Muted
		} else if settings.ring_volume == 0 {
			Speaker::Silent
		} else {
			Speaker::On
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
		self.directory
			.name_of_address(address)
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

fn push_limited(log: &mut VecDeque<String>, line: String) {
	if log.len() == LOG_LINES {
		log.pop_front();
	}
	log.push_back(line);
}

#[cfg(test)]
mod tests;
