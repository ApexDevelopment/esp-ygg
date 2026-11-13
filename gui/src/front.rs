//! Keeps the interface showing what the node reports, asks for the names of
//! the nodes on the page being looked at, and carries out what is asked in
//! the apps and on the settings page, and typed on the keyboard.

use alloc::collections::VecDeque;
#[cfg(any(feature = "console", feature = "irc", feature = "sftp"))]
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::Ipv6Addr;
#[cfg(feature = "console")]
use core::net::SocketAddrV6;

#[cfg(any(feature = "irc", feature = "gemini"))]
use esp_ygg_app::Stream;
use esp_ygg_app::settings::TextSetting;
use esp_ygg_app::time::{DateTime, format_offset, short_uptime};
use esp_ygg_app::ui::directory::Directory;
use esp_ygg_app::ui::{Command, Key, Status, WifiNetwork};
use esp_ygg_app::{Event, Settings};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use yggdrasil_espnow::PublicKey;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

#[cfg(feature = "console")]
use crate::console::Console;
#[cfg(feature = "gemini")]
use crate::gemini::Gemini;
#[cfg(feature = "gopher")]
use crate::gopher::Gopher;
#[cfg(feature = "irc")]
use crate::irc::Irc;
use crate::ping::Pinger;
#[cfg(feature = "sftp")]
use crate::sftp::SftpApp;
use crate::{
	App, Keyboard, Launcher, MainWindow, NeighborEntry, Node, NodeEntry, Notice, Ping, Setting,
	Setup, WifiEntry,
};

/// The most characters of the text being typed that the keyboard shows,
/// from the end, as fits its width.
const SHOWN_TYPED: usize = 24;
/// The longest an IPv6 address can be written.
const ADDRESS_LEN: usize = 45;
#[cfg(feature = "sftp")]
const SHARE_USER_LEN: usize = 32;
#[cfg(feature = "sftp")]
const SHARE_PASSWORD_LEN: usize = 64;
/// The longest an IPv6 address and port can be written.
#[cfg(feature = "console")]
const DESTINATION_LEN: usize = 53;
/// The longest message the console sends.
#[cfg(feature = "console")]
const MESSAGE_LEN: usize = 200;
#[cfg(feature = "gemini")]
const GEMINI_ADDRESS_LEN: usize = 120;
/// The longest answer to a Gemini server's question.
#[cfg(feature = "gemini")]
const GEMINI_ANSWER_LEN: usize = 200;
/// The longest Gopher address or search.
#[cfg(feature = "gopher")]
const GOPHER_LEN: usize = 60;
#[cfg(feature = "irc")]
const IRC_NICK_LEN: usize = 30;
/// The longest server or channel name.
#[cfg(feature = "irc")]
const IRC_NAME_LEN: usize = 52;
/// The longest IRC message, leaving room on the line for its command.
#[cfg(feature = "irc")]
const IRC_LINE_LEN: usize = esp_ygg_app::irc::MAX_LINE - 100;

/// What the interface asked for, until the next tick.
enum Action {
	Edit(TextSetting, SharedString),
	PickPingTarget(NodeEntry),
	TypePingAddress,
	StartPing,
	StopPing,
	/// Time for the next ping.
	SendPing,
	#[cfg(feature = "console")]
	PickConsoleTarget(NodeEntry),
	#[cfg(feature = "console")]
	TypeConsoleAddress,
	#[cfg(feature = "console")]
	WriteMessage,
	#[cfg(feature = "irc")]
	EditIrc(crate::IrcField),
	#[cfg(feature = "irc")]
	ConnectIrc,
	#[cfg(feature = "irc")]
	WriteIrc,
	#[cfg(feature = "irc")]
	SwitchIrc(SharedString),
	#[cfg(feature = "irc")]
	JoinIrc,
	#[cfg(feature = "irc")]
	DisconnectIrc,
	#[cfg(feature = "gopher")]
	GoGopher,
	#[cfg(feature = "gopher")]
	OpenGopher(i32),
	#[cfg(feature = "gopher")]
	BackGopher,
	#[cfg(feature = "gemini")]
	GoGemini,
	#[cfg(feature = "gemini")]
	OpenGemini(i32),
	#[cfg(feature = "gemini")]
	BackGemini,
	#[cfg(feature = "gemini")]
	StopGemini,
	#[cfg(feature = "sftp")]
	PickSftpTarget(NodeEntry),
	#[cfg(feature = "sftp")]
	TypeSftpAddress,
	#[cfg(feature = "sftp")]
	AdmitSftp(bool),
	PickNetwork(WifiEntry),
	Save,
	Export,
	Import,
	Scan,
	/// A character, or Backspace or Return, from the keyboard.
	Key(SharedString),
	Cancel,
}

/// What is being typed on the keyboard.
#[derive(Clone, Copy)]
enum Field {
	Setting(TextSetting),
	PingAddress,
	/// An address, with or without a port.
	#[cfg(feature = "console")]
	ConsoleAddress,
	#[cfg(feature = "console")]
	Message,
	#[cfg(feature = "irc")]
	IrcForm(crate::IrcField),
	/// A message or command to the IRC conversation showing.
	#[cfg(feature = "irc")]
	IrcLine,
	/// A channel to join.
	#[cfg(feature = "irc")]
	IrcJoin,
	#[cfg(feature = "gopher")]
	GopherAddress,
	/// The query for a Gopher search.
	#[cfg(feature = "gopher")]
	GopherSearch,
	#[cfg(feature = "gemini")]
	GeminiAddress,
	/// The answer to a Gemini server's question, and whether it is secret.
	#[cfg(feature = "gemini")]
	GeminiAnswer(bool),
	/// An address to send a shared file to, then who to log in as there.
	#[cfg(feature = "sftp")]
	SftpAddress,
	#[cfg(feature = "sftp")]
	SftpUser,
	/// The password a server asked for while sending.
	#[cfg(feature = "sftp")]
	SftpPassword,
}

impl Field {
	fn max_len(self) -> usize {
		match self {
			Field::Setting(setting) => setting.max_len(),
			Field::PingAddress => ADDRESS_LEN,
			#[cfg(feature = "console")]
			Field::ConsoleAddress => DESTINATION_LEN,
			#[cfg(feature = "console")]
			Field::Message => MESSAGE_LEN,
			#[cfg(feature = "irc")]
			Field::IrcForm(crate::IrcField::Nick) => IRC_NICK_LEN,
			#[cfg(feature = "irc")]
			Field::IrcForm(_) | Field::IrcJoin => IRC_NAME_LEN,
			#[cfg(feature = "irc")]
			Field::IrcLine => IRC_LINE_LEN,
			#[cfg(feature = "gopher")]
			Field::GopherAddress | Field::GopherSearch => GOPHER_LEN,
			#[cfg(feature = "gemini")]
			Field::GeminiAddress => GEMINI_ADDRESS_LEN,
			#[cfg(feature = "gemini")]
			Field::GeminiAnswer(_) => GEMINI_ANSWER_LEN,
			#[cfg(feature = "sftp")]
			Field::SftpAddress => ADDRESS_LEN,
			#[cfg(feature = "sftp")]
			Field::SftpUser => SHARE_USER_LEN,
			#[cfg(feature = "sftp")]
			Field::SftpPassword => SHARE_PASSWORD_LEN,
		}
	}

	fn is_secret(self) -> bool {
		match self {
			Field::Setting(setting) => setting.is_secret(),
			#[cfg(feature = "gemini")]
			Field::GeminiAnswer(secret) => secret,
			#[cfg(feature = "sftp")]
			Field::SftpPassword => true,
			_ => false,
		}
	}

	fn is_numeric(self) -> bool {
		matches!(self, Field::Setting(setting) if setting.is_numeric())
	}
}

struct Edit {
	field: Field,
	text: String,
}

pub struct Gui {
	window: MainWindow,
	directory: Directory,
	status: Option<Status>,
	nodes: Rc<VecModel<NodeEntry>>,
	neighbors: Rc<VecModel<NeighborEntry>>,
	networks: Rc<VecModel<WifiEntry>>,
	ping: Pinger,
	#[cfg(feature = "console")]
	console: Console,
	#[cfg(feature = "irc")]
	irc: Irc,
	#[cfg(feature = "gopher")]
	gopher: Gopher,
	#[cfg(feature = "gemini")]
	gemini: Gemini,
	#[cfg(feature = "sftp")]
	sftp: SftpApp,
	/// Where a typed address said to send, until a user is typed too.
	#[cfg(feature = "sftp")]
	sftp_to: Option<Ipv6Addr>,
	/// Whether the SFTP app was open, to list the folder as it opens.
	#[cfg(feature = "sftp")]
	sftp_was_open: bool,
	/// The settings in use.
	saved: Settings,
	/// The settings as edited, until saved.
	draft: Settings,
	editing: Option<Edit>,
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
		let actions = Rc::new(RefCell::new(VecDeque::new()));
		connect(&window, &actions);
		window
			.global::<Setup>()
			.set_networks(ModelRc::from(networks.clone()));
		let pings = actions.clone();
		let ping = Pinger::new(&window, move || {
			pings.borrow_mut().push_back(Action::SendPing);
		});
		window
			.global::<Launcher>()
			.set_apps(ModelRc::new(VecModel::from(Vec::from([
				App::Ping,
				#[cfg(feature = "console")]
				App::Console,
				#[cfg(feature = "irc")]
				App::Irc,
				#[cfg(feature = "gopher")]
				App::Gopher,
				#[cfg(feature = "gemini")]
				App::Gemini,
				#[cfg(feature = "sftp")]
				App::Sftp,
			]))));
		window.global::<Setup>().set_sftp(cfg!(feature = "sftp"));
		let gui = Gui {
			window: window.clone_strong(),
			directory: Directory::new(),
			status: None,
			nodes,
			neighbors,
			networks,
			ping,
			#[cfg(feature = "console")]
			console: Console::new(&window),
			#[cfg(feature = "irc")]
			irc: Irc::new(&window, &settings.name),
			#[cfg(feature = "gopher")]
			gopher: Gopher::new(&window),
			#[cfg(feature = "gemini")]
			gemini: Gemini::new(&window),
			#[cfg(feature = "sftp")]
			sftp: SftpApp::new(&window),
			#[cfg(feature = "sftp")]
			sftp_to: None,
			#[cfg(feature = "sftp")]
			sftp_was_open: false,
			draft: settings.clone(),
			saved: settings,
			editing: None,
			actions,
			commands: VecDeque::new(),
		};
		gui.show_settings();
		Ok(gui)
	}

	pub fn window(&self) -> &MainWindow {
		&self.window
	}

	/// Hides the interface behind a lock, or shows it again.
	pub fn set_locked(&self, locked: bool) {
		let lock = self.window.global::<crate::Lock>();
		if lock.get_locked() != locked {
			lock.set_locked(locked);
		}
	}

	pub fn set_status(&mut self, status: Status) {
		let node = self.window.global::<Node>();
		node.set_address(status.address.to_string().into());
		node.set_uptime(short_uptime(status.board.uptime.as_secs()).into());
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

	/// Takes what became of what the apps asked the network for.
	pub fn handle_event(&mut self, event: Event) {
		match event {
			Event::PingReply {
				from,
				sequence,
				round_trip,
			} => self.ping.replied(from, sequence, round_trip),
			Event::PingTimeout { target, sequence } => self.ping.timed_out(target, sequence),
			#[cfg(feature = "console")]
			Event::Datagram { from, data } => {
				let label = self.label_of(*from.ip());
				let viewing = self.viewing(App::Console);
				if !viewing {
					self.notify(format!(
						"{label}: {}",
						String::from_utf8_lossy(&data).trim_end()
					));
				}
				self.console.received(from, label, &data, viewing);
			}
			#[cfg(feature = "irc")]
			Event::StreamOpened(Stream::Irc) => self.irc.opened(&mut self.commands),
			#[cfg(feature = "irc")]
			Event::StreamData {
				stream: Stream::Irc,
				data,
			} => {
				self.irc.refresh(self.viewing_irc());
				self.irc.received(&data, &mut self.commands);
			}
			#[cfg(feature = "irc")]
			Event::StreamClosed(Stream::Irc) => self.irc.closed(),
			#[cfg(feature = "gopher")]
			Event::Fetched { data } => self.gopher.fetched(&data),
			#[cfg(feature = "gopher")]
			Event::FetchFailed => self.gopher.failed(),
			#[cfg(feature = "gemini")]
			Event::StreamOpened(Stream::Gemini) => self.gemini.opened(&mut self.commands),
			#[cfg(feature = "gemini")]
			Event::StreamData {
				stream: Stream::Gemini,
				data,
			} => self.gemini.received(&data, &mut self.commands),
			#[cfg(feature = "gemini")]
			Event::StreamClosed(Stream::Gemini) => self.gemini.closed(&mut self.commands),
			// Other apps' events, in builds with them
			#[allow(unreachable_patterns)]
			_ => {}
		}
	}

	/// Carries out what the interface asked for, and looks up the names of
	/// nodes on the page showing.
	pub fn tick(&mut self) {
		let actions: Vec<Action> = self.actions.borrow_mut().drain(..).collect();
		for action in actions {
			self.carry_out(action);
		}
		#[cfg(feature = "console")]
		// The notice of a new message would only cover it.
		if self.viewing(App::Console) && self.console.mark_read() {
			self.notify(String::new());
		}
		#[cfg(feature = "irc")]
		self.irc.refresh(self.viewing_irc());
		#[cfg(feature = "gopher")]
		self.gopher.refresh();
		#[cfg(feature = "gemini")]
		if let Some((question, secret)) = self.gemini.refresh() {
			self.open_keyboard(Field::GeminiAnswer(secret), question.into());
		}
		#[cfg(feature = "sftp")]
		self.tick_sftp();
		// The switches change the edits in the interface alone.
		if self.window.get_page() == 3 {
			self.take_switches();
			self.show_restart();
		}
		let Some(status) = &self.status else {
			return;
		};
		let choosing = match self.window.global::<Launcher>().get_open() {
			App::Ping => self.window.global::<Ping>().get_choosing(),
			App::Console => self.window.global::<crate::Console>().get_choosing(),
			App::Sftp => self.window.global::<crate::Sftp>().get_sending() != "",
			App::Irc | App::Gopher | App::Gemini | App::None => false,
		};
		let wanted: Vec<PublicKey> = match self.window.get_page() {
			0 if choosing => status.nodes(),
			1 => status.nodes(),
			2 => status.neighbors.iter().filter_map(|n| n.key).collect(),
			_ => Vec::new(),
		};
		for key in self.directory.start_lookups(wanted) {
			self.commands.push_back(Command::RequestNodeInfo(key));
		}
	}

	pub fn poll_command(&mut self) -> Option<Command> {
		self.commands.pop_front()
	}

	/// Types a key from a keyboard other than the screen's, such as the
	/// serial port's, into the setting being edited, if there is one.
	pub fn type_key(&mut self, key: Key) {
		match key {
			Key::Char(character) => self.type_text(&character.to_string()),
			Key::Backspace => self.backspace(),
			Key::Enter => self.finish_edit(),
			Key::Escape => self.close_keyboard(),
			_ => {}
		}
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
			Ok(()) => "Exported".into(),
			Err(problem) => problem,
		});
	}

	/// The settings file read from the SD card, or why it could not be.
	/// What it holds replaces the edits, until they are saved.
	pub fn settings_imported(&mut self, result: Result<String, String>) {
		self.take_switches();
		match result.and_then(|text| self.draft.with_text(&text)) {
			Ok(settings) => {
				self.draft = settings;
				self.show_settings();
				self.notify("Imported, not saved".into());
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
			Action::Edit(setting, label) => self.open_keyboard(Field::Setting(setting), label),
			Action::PickPingTarget(node) => {
				if let Some(address) = address_of(&node) {
					let command = self.ping.set_target(address, node.name.into());
					self.commands.extend(command);
				}
			}
			#[cfg(feature = "console")]
			Action::PickConsoleTarget(node) => {
				if let Some(address) = address_of(&node) {
					let to = SocketAddrV6::new(address, esp_ygg_app::CONSOLE_PORT, 0, 0);
					self.console.set_target(to, node.name.into());
				}
			}
			#[cfg(feature = "console")]
			Action::TypeConsoleAddress => self.open_keyboard(Field::ConsoleAddress, "Address".into()),
			#[cfg(feature = "irc")]
			Action::EditIrc(field) => {
				let label = match field {
					crate::IrcField::Server => "Server",
					crate::IrcField::Nick => "Nick",
					crate::IrcField::Channel => "Channel",
				};
				self.open_keyboard(Field::IrcForm(field), label.into());
			}
			#[cfg(feature = "irc")]
			Action::ConnectIrc => self.irc.connect(&mut self.commands),
			#[cfg(feature = "irc")]
			Action::WriteIrc => {
				let label = self.irc.current().into();
				self.open_keyboard(Field::IrcLine, label);
			}
			#[cfg(feature = "irc")]
			Action::SwitchIrc(name) => self.irc.switch_to(&name),
			#[cfg(feature = "irc")]
			Action::JoinIrc => self.open_keyboard(Field::IrcJoin, "Channel".into()),
			#[cfg(feature = "irc")]
			Action::DisconnectIrc => self.irc.submit("/quit", &mut self.commands),
			#[cfg(feature = "gopher")]
			Action::GoGopher => self.open_keyboard(Field::GopherAddress, "Address".into()),
			#[cfg(feature = "gopher")]
			Action::OpenGopher(link) => {
				let link = usize::try_from(link).unwrap_or(usize::MAX);
				if self.gopher.open(link, &mut self.commands) {
					self.open_keyboard(Field::GopherSearch, "Search".into());
				}
			}
			#[cfg(feature = "gopher")]
			Action::BackGopher => self.gopher.back(),
			#[cfg(feature = "gemini")]
			Action::GoGemini => self.open_keyboard(Field::GeminiAddress, "Address".into()),
			#[cfg(feature = "gemini")]
			Action::OpenGemini(link) => {
				let link = usize::try_from(link).unwrap_or(usize::MAX);
				self.gemini.open(link, &mut self.commands);
			}
			#[cfg(feature = "gemini")]
			Action::BackGemini => self.gemini.back(&mut self.commands),
			#[cfg(feature = "gemini")]
			Action::StopGemini => self.gemini.stop(&mut self.commands),
			#[cfg(feature = "sftp")]
			Action::PickSftpTarget(node) => {
				if let (Some(to), Some(name)) = (address_of(&node), self.sftp.sending()) {
					self.commands.push_back(Command::SftpSend {
						to,
						user: self.sftp.user.clone(),
						name: name.into(),
					});
				}
				self.sftp.stop_choosing();
			}
			#[cfg(feature = "sftp")]
			Action::TypeSftpAddress => self.open_keyboard(Field::SftpAddress, "Address".into()),
			#[cfg(feature = "sftp")]
			Action::AdmitSftp(admit) => self.commands.push_back(Command::SftpAdmit(admit)),
			#[cfg(feature = "console")]
			Action::WriteMessage => {
				let label = self.window.global::<crate::Console>().get_target();
				self.open_keyboard(Field::Message, format!("To {label}").into());
			}
			Action::TypePingAddress => self.open_keyboard(Field::PingAddress, "Address".into()),
			Action::StartPing => {
				let command = self.ping.start();
				self.commands.extend(command);
			}
			Action::StopPing => self.ping.stop(),
			Action::SendPing => {
				let command = self.ping.send();
				self.commands.extend(command);
			}
			Action::PickNetwork(network) => {
				self.draft.ssid = network.ssid.into();
				if !network.secured {
					self.draft.password.clear();
				}
				self.window.global::<Setup>().set_choosing_network(false);
				self.show_settings();
			}
			Action::Save => {
				self.take_switches();
				if self.draft.bridge && !self.draft.can_bridge() {
					self.notify("Bridge needs network and peer".into());
					return;
				}
				let restarting = self.saved.needs_restart(&self.draft);
				self.saved = self.draft.clone();
				self.commands.push_back(Command::Save(self.draft.clone()));
				self.notify(if restarting { "Restarting" } else { "Saved" }.into());
				self.show_restart();
			}
			Action::Export => {
				self.take_switches();
				self.commands
					.push_back(Command::ExportSettings(self.draft.to_text()));
			}
			Action::Import => self.commands.push_back(Command::ImportSettings),
			Action::Scan => {
				sync(&self.networks, Vec::new());
				self.window.global::<Setup>().set_scanning(true);
				self.commands.push_back(Command::ScanWifi);
			}
			Action::Key(text) if text.as_str() == "\u{8}" => self.backspace(),
			Action::Key(text) if text.as_str() == "\n" => self.finish_edit(),
			Action::Key(text) => self.type_text(&text),
			Action::Cancel => self.close_keyboard(),
		}
	}

	fn open_keyboard(&mut self, field: Field, label: SharedString) {
		let text = match field {
			Field::Setting(setting) => self.draft.text(setting),
			Field::PingAddress => self
				.ping
				.target()
				.map(|a| a.to_string())
				.unwrap_or_default(),
			#[cfg(feature = "console")]
			Field::ConsoleAddress => self
				.console
				.target()
				.map(|to| to.to_string())
				.unwrap_or_default(),
			#[cfg(feature = "console")]
			Field::Message => String::new(),
			#[cfg(feature = "irc")]
			Field::IrcForm(field) => self.irc.form(field),
			#[cfg(feature = "irc")]
			Field::IrcLine | Field::IrcJoin => String::new(),
			#[cfg(feature = "gopher")]
			Field::GopherAddress => self.gopher.address(),
			#[cfg(feature = "gopher")]
			Field::GopherSearch => String::new(),
			#[cfg(feature = "gemini")]
			Field::GeminiAddress => self.gemini.address(),
			#[cfg(feature = "gemini")]
			Field::GeminiAnswer(_) => String::new(),
			#[cfg(feature = "sftp")]
			Field::SftpUser => self.sftp.user.clone(),
			#[cfg(feature = "sftp")]
			Field::SftpAddress | Field::SftpPassword => String::new(),
		};
		self.editing = Some(Edit { field, text });
		let keyboard = self.window.global::<Keyboard>();
		keyboard.set_label(label);
		keyboard.set_error(SharedString::new());
		keyboard.set_numeric(field.is_numeric());
		keyboard.set_open(true);
		self.show_typed();
	}

	fn type_text(&mut self, text: &str) {
		let Some(edit) = &mut self.editing else {
			return;
		};
		for character in text.chars() {
			if edit.text.chars().count() < edit.field.max_len() {
				edit.text.push(character);
			}
		}
		self.show_typed();
	}

	fn backspace(&mut self) {
		if let Some(edit) = &mut self.editing {
			edit.text.pop();
			self.show_typed();
		}
	}

	/// Applies the text typed if it will do, or says why not and leaves the
	/// keyboard up.
	fn finish_edit(&mut self) {
		let Some(edit) = &self.editing else {
			return;
		};
		let applied = match edit.field {
			Field::Setting(setting) => self.draft.set_text(setting, &edit.text),
			Field::PingAddress => match edit.text.parse::<Ipv6Addr>() {
				Ok(address) => {
					let command = self.ping.set_target(address, self.label_of(address));
					self.commands.extend(command);
					Ok(())
				}
				Err(_) => Err("Not an IPv6 address"),
			},
			#[cfg(feature = "console")]
			Field::ConsoleAddress => match esp_ygg_app::ui::console::parse_destination(&edit.text) {
				Some(to) => {
					let label = self.label_of(*to.ip());
					self.console.set_target(to, label);
					Ok(())
				}
				None => Err("Not an address"),
			},
			#[cfg(feature = "console")]
			Field::Message => {
				let command = self.console.send(&edit.text);
				self.commands.extend(command);
				Ok(())
			}
			#[cfg(feature = "irc")]
			Field::IrcForm(field) => {
				self.irc.set_form(field, &edit.text);
				Ok(())
			}
			#[cfg(feature = "irc")]
			Field::IrcLine => {
				let text = edit.text.clone();
				self.irc.submit(&text, &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "irc")]
			Field::IrcJoin => {
				let channel = edit.text.trim();
				let channel = if channel.starts_with(['#', '&']) {
					String::from(channel)
				} else {
					format!("#{channel}")
				};
				self.irc
					.submit(&format!("/join {channel}"), &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "gopher")]
			Field::GopherAddress => {
				let text = edit.text.clone();
				self.gopher.go_to(&text, &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "gopher")]
			Field::GopherSearch => {
				let query = edit.text.clone();
				self.gopher.search(&query, &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "gemini")]
			Field::GeminiAddress => {
				let text = edit.text.clone();
				self.gemini.go_to(&text, &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "gemini")]
			Field::GeminiAnswer(_) => {
				let answer = edit.text.clone();
				self.gemini.answer(&answer, &mut self.commands);
				Ok(())
			}
			#[cfg(feature = "sftp")]
			Field::SftpAddress => match edit.text.trim().parse::<Ipv6Addr>() {
				Ok(to) => {
					self.sftp_to = Some(to);
					Ok(())
				}
				Err(_) => Err("Not an IPv6 address"),
			},
			#[cfg(feature = "sftp")]
			Field::SftpUser => {
				self.sftp.user = edit.text.trim().into();
				if let (Some(to), Some(name)) = (self.sftp_to.take(), self.sftp.sending()) {
					self.commands.push_back(Command::SftpSend {
						to,
						user: self.sftp.user.clone(),
						name: name.into(),
					});
				}
				self.sftp.stop_choosing();
				Ok(())
			}
			#[cfg(feature = "sftp")]
			Field::SftpPassword => {
				let password = edit.text.clone();
				self.commands
					.push_back(Command::SftpPassword(Some(password)));
				self.sftp.password_answered();
				Ok(())
			}
		};
		#[cfg(feature = "sftp")]
		let then_user = matches!(edit.field, Field::SftpAddress) && applied.is_ok();
		match applied {
			Ok(()) => {
				self.close_keyboard();
				self.show_settings();
				#[cfg(feature = "sftp")]
				if then_user {
					self.open_keyboard(Field::SftpUser, "User".into());
				}
			}
			Err(problem) => self.window.global::<Keyboard>().set_error(problem.into()),
		}
	}

	fn close_keyboard(&mut self) {
		#[cfg(feature = "gopher")]
		if let Some(Edit {
			field: Field::GopherSearch,
			..
		}) = self.editing
		{
			self.gopher.cancel_search();
		}
		#[cfg(feature = "gemini")]
		if let Some(Edit {
			field: Field::GeminiAnswer(_),
			..
		}) = self.editing
		{
			self.gemini.cancel_answer();
		}
		// Giving up on the password gives up on sending.
		#[cfg(feature = "sftp")]
		if let Some(Edit {
			field: Field::SftpPassword,
			..
		}) = self.editing
			&& self.sftp.password_for().is_some()
		{
			self.commands.push_back(Command::SftpPassword(None));
			self.sftp.password_answered();
		}
		self.editing = None;
		self.window.global::<Keyboard>().set_open(false);
	}

	/// Shows the end of the text being typed on the keyboard.
	fn show_typed(&self) {
		let Some(edit) = &self.editing else {
			return;
		};
		let shown = shown_value(edit.field, &edit.text);
		let count = shown.chars().count();
		let end: String = shown
			.chars()
			.skip(count.saturating_sub(SHOWN_TYPED))
			.collect();
		let keyboard = self.window.global::<Keyboard>();
		keyboard.set_text(end.into());
		keyboard.set_error(SharedString::new());
	}

	/// Takes what the switches on the settings page say into the edits.
	fn take_switches(&mut self) {
		let setup = self.window.global::<Setup>();
		self.draft.status_page = setup.get_status_pages();
		self.draft.bridge = setup.get_bridge();
		self.draft.hotspot = setup.get_hotspot();
	}

	/// Says on the save button whether saving the edits restarts the board.
	fn show_restart(&self) {
		let setup = self.window.global::<Setup>();
		let restart = self.saved.needs_restart(&self.draft);
		if setup.get_restart() != restart {
			setup.set_restart(restart);
		}
	}

	/// Shows the edits on the settings page.
	fn show_settings(&self) {
		let setup = self.window.global::<Setup>();
		let draft = &self.draft;
		setup.set_name(draft.name.as_str().into());
		setup.set_channel(draft.channel.to_string().into());
		setup.set_time_zone(format_offset(draft.utc_offset).into());
		setup.set_ssid(draft.ssid.as_str().into());
		let wifi_password = shown_value(Field::Setting(TextSetting::WifiPassword), &draft.password);
		setup.set_wifi_password(wifi_password.into());
		setup.set_peer(draft.peer.as_str().into());
		setup.set_hotspot_name(draft.hotspot_name.as_str().into());
		let hotspot_password = shown_value(
			Field::Setting(TextSetting::HotspotPassword),
			&draft.hotspot_password,
		);
		setup.set_hotspot_password(hotspot_password.into());
		let sftp_password = shown_value(
			Field::Setting(TextSetting::SftpPassword),
			&draft.sftp_password,
		);
		setup.set_sftp_password(sftp_password.into());
		setup.set_status_pages(draft.status_page);
		setup.set_bridge(draft.bridge);
		setup.set_hotspot(draft.hotspot);
	}

	/// A node's name if known, or its address.
	fn label_of(&self, address: Ipv6Addr) -> String {
		match self.directory.name_of_address(&address) {
			Some(name) => name.into(),
			None => address.to_string(),
		}
	}

	/// Shows what SFTP is doing, from the board's SFTP tasks.
	#[cfg(feature = "sftp")]
	pub fn set_sftp_status(&mut self, status: esp_ygg_app::sftp::Status) {
		let directory = &self.directory;
		let label = |address: &Ipv6Addr| match directory.name_of_address(address) {
			Some(name) => name.into(),
			None => address.to_string(),
		};
		if self.sftp.set_status(status, label) {
			self.commands.push_back(Command::SftpList);
		}
	}

	/// The shared folder's files, as asked for with [`Command::ShareList`].
	#[cfg(feature = "sftp")]
	pub fn set_sftp_files(&mut self, files: Vec<esp_ygg_app::sftp::Entry>) {
		self.sftp.set_files(files);
	}

	/// Lists the folder as SFTP opens, and asks for a password while a
	/// server wants one.
	#[cfg(feature = "sftp")]
	fn tick_sftp(&mut self) {
		let open = self.window.global::<Launcher>().get_open() == App::Sftp;
		if open && !self.sftp_was_open {
			self.commands.push_back(Command::SftpList);
		}
		self.sftp_was_open = open;
		let typing = matches!(
			self.editing,
			Some(Edit {
				field: Field::SftpPassword,
				..
			})
		);
		match self.sftp.password_for() {
			Some(user) if !typing => {
				let label = format!("Password for {user}");
				self.open_keyboard(Field::SftpPassword, label.into());
			}
			// The server gave up asking.
			None if typing => {
				self.editing = None;
				self.window.global::<Keyboard>().set_open(false);
			}
			_ => {}
		}
	}

	/// Whether a conversation in IRC is showing.
	#[cfg(feature = "irc")]
	fn viewing_irc(&self) -> bool {
		self.viewing(App::Irc) && !self.window.global::<crate::Irc>().get_choosing()
	}

	/// Whether `app` is open and showing.
	#[cfg(any(feature = "console", feature = "irc"))]
	fn viewing(&self, app: App) -> bool {
		self.window.get_page() == 0 && self.window.global::<Launcher>().get_open() == app
	}

	fn notify(&self, text: String) {
		self.window.global::<Notice>().set_text(text.into());
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

/// Queues what the interface's callbacks ask for, for the next tick.
fn connect(window: &MainWindow, actions: &Rc<RefCell<VecDeque<Action>>>) {
	let queue = |action: fn() -> Action| {
		let actions = actions.clone();
		move || actions.borrow_mut().push_back(action())
	};
	let setup = window.global::<Setup>();
	setup.on_save(queue(|| Action::Save));
	setup.on_export(queue(|| Action::Export));
	setup.on_import(queue(|| Action::Import));
	setup.on_scan(queue(|| Action::Scan));
	let edits = actions.clone();
	setup.on_edit(move |setting, label| {
		let action = Action::Edit(text_setting(setting), label);
		edits.borrow_mut().push_back(action);
	});
	let picks = actions.clone();
	setup
		.on_pick_network(move |network| picks.borrow_mut().push_back(Action::PickNetwork(network)));
	let ping = window.global::<Ping>();
	ping.on_type_address(queue(|| Action::TypePingAddress));
	ping.on_start(queue(|| Action::StartPing));
	ping.on_stop(queue(|| Action::StopPing));
	let pings = actions.clone();
	ping.on_pick(move |node| pings.borrow_mut().push_back(Action::PickPingTarget(node)));
	#[cfg(feature = "console")]
	{
		let console = window.global::<crate::Console>();
		console.on_type_address(queue(|| Action::TypeConsoleAddress));
		console.on_write(queue(|| Action::WriteMessage));
		let picks = actions.clone();
		console.on_pick(move |node| {
			picks
				.borrow_mut()
				.push_back(Action::PickConsoleTarget(node))
		});
	}
	#[cfg(feature = "irc")]
	{
		let irc = window.global::<crate::Irc>();
		irc.on_connect(queue(|| Action::ConnectIrc));
		irc.on_write(queue(|| Action::WriteIrc));
		irc.on_join(queue(|| Action::JoinIrc));
		irc.on_disconnect(queue(|| Action::DisconnectIrc));
		let edits = actions.clone();
		irc.on_edit(move |field| edits.borrow_mut().push_back(Action::EditIrc(field)));
		let switches = actions.clone();
		irc.on_switch_to(move |name| switches.borrow_mut().push_back(Action::SwitchIrc(name)));
	}
	#[cfg(feature = "gopher")]
	{
		let gopher = window.global::<crate::Gopher>();
		gopher.on_go(queue(|| Action::GoGopher));
		gopher.on_back(queue(|| Action::BackGopher));
		let opens = actions.clone();
		gopher.on_open(move |link| opens.borrow_mut().push_back(Action::OpenGopher(link)));
	}
	#[cfg(feature = "gemini")]
	{
		let gemini = window.global::<crate::Gemini>();
		gemini.on_go(queue(|| Action::GoGemini));
		gemini.on_back(queue(|| Action::BackGemini));
		gemini.on_stop(queue(|| Action::StopGemini));
		let opens = actions.clone();
		gemini.on_open(move |link| opens.borrow_mut().push_back(Action::OpenGemini(link)));
	}
	#[cfg(feature = "sftp")]
	{
		let sftp = window.global::<crate::Sftp>();
		sftp.on_type_address(queue(|| Action::TypeSftpAddress));
		let picks = actions.clone();
		sftp.on_pick(move |node| picks.borrow_mut().push_back(Action::PickSftpTarget(node)));
		let admits = actions.clone();
		sftp.on_admit(move |admit| admits.borrow_mut().push_back(Action::AdmitSftp(admit)));
	}
	let keyboard = window.global::<Keyboard>();
	let keys = actions.clone();
	keyboard.on_key(move |text| keys.borrow_mut().push_back(Action::Key(text)));
	let rows = actions.clone();
	keyboard.on_key_at(move |row, index, upper| {
		let Some(character) = usize::try_from(index).ok().and_then(|i| row.chars().nth(i)) else {
			return;
		};
		let text: SharedString = if upper {
			character.to_uppercase().collect::<String>().into()
		} else {
			character.to_string().into()
		};
		rows.borrow_mut().push_back(Action::Key(text));
	});
	keyboard.on_cancel(queue(|| Action::Cancel));
}

/// A node's address, from its row in a list.
fn address_of(node: &NodeEntry) -> Option<Ipv6Addr> {
	let address = if node.address.is_empty() {
		&node.name
	} else {
		&node.address
	};
	address.parse().ok()
}

fn text_setting(setting: Setting) -> TextSetting {
	match setting {
		Setting::Name => TextSetting::Name,
		Setting::Channel => TextSetting::Channel,
		Setting::TimeZone => TextSetting::TimeZone,
		Setting::Ssid => TextSetting::Ssid,
		Setting::WifiPassword => TextSetting::WifiPassword,
		Setting::Peer => TextSetting::Peer,
		Setting::HotspotName => TextSetting::HotspotName,
		Setting::HotspotPassword => TextSetting::HotspotPassword,
		Setting::SftpPassword => TextSetting::SftpPassword,
	}
}

/// What is typed as shown, with dots for a password.
fn shown_value(field: Field, text: &str) -> String {
	if field.is_secret() {
		"●".repeat(text.chars().count())
	} else {
		text.into()
	}
}

/// Changes `model` to hold `rows`, touching only rows that differ, so the
/// lists keep their place and only what changed is drawn again.
pub(crate) fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
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
