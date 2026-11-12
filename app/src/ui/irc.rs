//! An IRC client: a form to connect, then one buffer of lines for the
//! server and for each channel or private conversation.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::view::{ACCENT, DIM, field_line, split_bottom, split_top, wrap};

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};

use super::{Command, Commands, Key, Outcome, TextInput};
use crate::irc::{self, LineReader, Message};
use crate::overlay::Stream;

const MAX_LINES: usize = 100;
const MAX_BUFFERS: usize = 8;
const SERVER: &str = "server";

pub(super) struct Buffer {
	pub name: String,
	pub lines: VecDeque<String>,
	pub unread: usize,
}

impl Buffer {
	fn new(name: &str) -> Self {
		Buffer {
			name: name.into(),
			lines: VecDeque::new(),
			unread: 0,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
	Setup,
	Connecting,
	Connected,
}

pub(super) struct Client {
	pub phase: Phase,
	/// The server, nick and channel to join.
	pub form: [TextInput; 3],
	pub field: usize,
	pub buffers: Vec<Buffer>,
	pub current: usize,
	pub input: TextInput,
	/// How many lines the view is scrolled up from the newest.
	pub scroll: usize,
	pub nick: String,
	pub error: Option<String>,
	reader: LineReader,
	registered: bool,
	join_after_welcome: Option<String>,
}

impl Client {
	pub fn new(nick: &str) -> Self {
		Client {
			phase: Phase::Setup,
			form: [
				TextInput::new("", 52),
				TextInput::new(nick, 30),
				TextInput::new("#esp-ygg", 50),
			],
			field: 0,
			buffers: Vec::from([Buffer::new(SERVER)]),
			current: 0,
			input: TextInput::new("", irc::MAX_LINE - 100),
			scroll: 0,
			nick: nick.into(),
			error: None,
			reader: LineReader::default(),
			registered: false,
			join_after_welcome: None,
		}
	}

	pub fn mark_read(&mut self) {
		self.buffers[self.current].unread = 0;
	}

	/// Lines that arrived in buffers no one has looked at since.
	pub fn unread(&self) -> usize {
		self.buffers.iter().map(|buffer| buffer.unread).sum()
	}

	pub fn handle_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		match self.phase {
			Phase::Setup => self.form_key(key, commands),
			Phase::Connecting | Phase::Connected => self.chat_key(key, commands),
		}
	}

	fn form_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		match key {
			Key::Escape => return Outcome::Leave,
			Key::Up => self.field = self.field.saturating_sub(1),
			Key::Down | Key::Tab => self.field = (self.field + 1) % self.form.len(),
			Key::Enter if self.field + 1 < self.form.len() => self.field += 1,
			Key::Enter => self.connect(commands),
			Key::Backspace => self.form[self.field].backspace(),
			Key::Char(character) => self.form[self.field].push(character),
			_ => {}
		}
		Outcome::Stay
	}

	fn connect(&mut self, commands: &mut Commands) {
		let Some(server) = parse_server(&self.form[0].text) else {
			self.error = Some("Enter the server as an address or [address]:port".into());
			self.field = 0;
			return;
		};
		let nick = self.form[1].text.trim();
		if nick.is_empty() || nick.contains(' ') {
			self.error = Some("Choose a nick without spaces".into());
			self.field = 1;
			return;
		}
		self.nick = nick.into();
		let channel = self.form[2].text.trim();
		self.join_after_welcome = (!channel.is_empty()).then(|| channel.into());
		self.error = None;
		self.registered = false;
		self.reader = LineReader::default();
		self.buffers = Vec::from([Buffer::new(SERVER)]);
		self.current = 0;
		self.phase = Phase::Connecting;
		self.log(SERVER, format!("Connecting to {server}"), true);
		commands.push_back(Command::OpenStream(Stream::Irc, server));
	}

	fn chat_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		match key {
			Key::Escape => return Outcome::Leave,
			Key::Tab => {
				self.current = (self.current + 1) % self.buffers.len();
				self.buffers[self.current].unread = 0;
				self.scroll = 0;
			}
			Key::Up => self.scroll += 1,
			Key::Down => self.scroll = self.scroll.saturating_sub(1),
			Key::Enter => {
				let text = core::mem::take(&mut self.input.text);
				self.submit(text.trim(), commands);
			}
			Key::Backspace => self.input.backspace(),
			Key::Char(character) => self.input.push(character),
			_ => {}
		}
		Outcome::Stay
	}

	/// Sends what was typed: a message to the current buffer, or a command.
	fn submit(&mut self, text: &str, commands: &mut Commands) {
		if text.is_empty() {
			return;
		}
		let target = self.buffers[self.current].name.clone();
		let Some(line) = text.strip_prefix('/') else {
			if target == SERVER {
				self.log(SERVER, "Join a channel with /join #channel".into(), true);
				return;
			}
			send(commands, irc::command("PRIVMSG", &[&target], Some(text)));
			let nick = self.nick.clone();
			self.log(&target, format!("<{nick}> {text}"), true);
			return;
		};
		let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
		let rest = rest.trim();
		match name.to_ascii_lowercase().as_str() {
			"join" if !rest.is_empty() => send(commands, irc::command("JOIN", &[rest], None)),
			"part" => {
				let channel = if rest.is_empty() {
					target.as_str()
				} else {
					rest
				};
				send(commands, irc::command("PART", &[channel], None));
			}
			"msg" => match rest.split_once(' ') {
				Some((to, message)) => {
					send(commands, irc::command("PRIVMSG", &[to], Some(message)));
					let nick = self.nick.clone();
					self.log(to, format!("<{nick}> {message}"), true);
					self.switch_to(to);
				}
				None => self.log(&target, "Use /msg nick message".into(), true),
			},
			"me" if target != SERVER => {
				send(commands, irc::action(&target, rest));
				let nick = self.nick.clone();
				self.log(&target, format!("* {nick} {rest}"), true);
			}
			"nick" if !rest.is_empty() => send(commands, irc::command("NICK", &[rest], None)),
			"quit" => {
				let message = if rest.is_empty() { "Bye" } else { rest };
				send(commands, irc::command("QUIT", &[], Some(message)));
				commands.push_back(Command::CloseStream(Stream::Irc));
			}
			"quote" if !rest.is_empty() => {
				let mut raw = rest.to_string().into_bytes();
				raw.extend_from_slice(b"\r\n");
				send(commands, raw);
			}
			_ => self.log(&target, format!("Unknown command: /{name}"), true),
		}
	}

	pub fn opened(&mut self, commands: &mut Commands) {
		self.phase = Phase::Connected;
		let nick = self.nick.clone();
		send(commands, irc::command("NICK", &[&nick], None));
		send(
			commands,
			irc::command("USER", &[&nick, "0", "*"], Some("esp-ygg")),
		);
	}

	pub fn closed(&mut self) {
		if self.phase != Phase::Setup {
			self.log(SERVER, "Disconnected".into(), true);
			self.error = Some("Disconnected".into());
		}
		self.phase = Phase::Setup;
	}

	pub fn received(&mut self, data: &[u8], viewing: bool, commands: &mut Commands) {
		for line in self.reader.push(data) {
			if let Some(message) = Message::parse(&line) {
				self.handle_message(&message, viewing, commands);
			}
		}
	}

	fn handle_message(&mut self, message: &Message, viewing: bool, commands: &mut Commands) {
		let from = message.nick().unwrap_or(SERVER).to_string();
		match message.command.as_str() {
			"PING" => send(commands, irc::command("PONG", &[], Some(message.text()))),
			"001" => {
				self.registered = true;
				self.nick = message.param(0).into();
				self.log(SERVER, message.text().into(), viewing);
				if let Some(channel) = self.join_after_welcome.take() {
					send(commands, irc::command("JOIN", &[&channel], None));
				}
			}
			"433" if !self.registered => {
				self.nick.push('_');
				let nick = self.nick.clone();
				send(commands, irc::command("NICK", &[&nick], None));
			}
			"PRIVMSG" | "NOTICE" => {
				let target = message.param(0);
				let buffer = if irc::is_channel(target) {
					target.to_string()
				} else if from == SERVER || !self.registered || message.prefix.is_none() {
					SERVER.into()
				} else {
					from.clone()
				};
				let line = match irc::action_text(message.text()) {
					Some(action) => format!("* {from} {action}"),
					None if message.command == "NOTICE" => format!("-{from}- {}", message.text()),
					None => format!("<{from}> {}", message.text()),
				};
				self.log(&buffer, line, viewing);
			}
			"JOIN" => {
				let channel = if message.params.is_empty() {
					""
				} else {
					message.param(0)
				};
				if from == self.nick {
					self.switch_to(channel);
				}
				self.log(channel, format!("* {from} joined"), viewing);
			}
			"PART" => {
				let channel = message.param(0);
				let reason = message.params.get(1).map(|reason| format!(" ({reason})"));
				let line = format!("* {from} left{}", reason.unwrap_or_default());
				self.log(channel, line, viewing);
			}
			"QUIT" => {
				let current = self.buffers[self.current].name.clone();
				let line = format!("* {from} quit ({})", message.text());
				self.log(&current, line, viewing);
			}
			"NICK" => {
				let new = message.text().to_string();
				if from == self.nick {
					self.nick = new.clone();
				}
				let current = self.buffers[self.current].name.clone();
				self.log(&current, format!("* {from} is now {new}"), viewing);
			}
			"332" => {
				let line = format!("Topic: {}", message.text());
				self.log(message.param(1), line, viewing);
			}
			"353" => {
				let line = format!("Here: {}", message.text());
				self.log(message.param(2), line, viewing);
			}
			// The end of the names list and other noise.
			"366" | "333" => {}
			_ => {
				let text = if message.command.starts_with(|c: char| c.is_ascii_digit()) {
					message.text().to_string()
				} else {
					format!("{} {}", message.command, message.params.join(" "))
				};
				self.log(SERVER, text, viewing);
			}
		}
	}

	fn switch_to(&mut self, name: &str) {
		let index = self.buffer_index(name);
		self.current = index;
		self.buffers[index].unread = 0;
		self.scroll = 0;
	}

	fn buffer_index(&mut self, name: &str) -> usize {
		if let Some(index) = self
			.buffers
			.iter()
			.position(|buffer| buffer.name.eq_ignore_ascii_case(name))
		{
			return index;
		}
		if self.buffers.len() == MAX_BUFFERS {
			// Drop the oldest conversation, but never the server's.
			self.buffers.remove(1);
			self.current = match self.current {
				1 => 0,
				current if current > 1 => current - 1,
				current => current,
			};
		}
		self.buffers.push(Buffer::new(name));
		self.buffers.len() - 1
	}

	fn log(&mut self, buffer: &str, line: String, viewing: bool) {
		let line = irc::strip_formatting(&line);
		let name = if buffer.is_empty() { SERVER } else { buffer };
		let index = self.buffer_index(name);
		let entry = &mut self.buffers[index];
		if entry.lines.len() == MAX_LINES {
			entry.lines.pop_front();
		}
		entry.lines.push_back(line);
		if !(viewing && index == self.current) {
			entry.unread += 1;
		}
	}
}

fn send(commands: &mut Commands, line: Vec<u8>) {
	commands.push_back(Command::StreamSend(Stream::Irc, line));
}

/// `[address]:port`, or a bare address for the usual IRC port.
fn parse_server(text: &str) -> Option<SocketAddrV6> {
	let text = text.trim();
	text.parse::<SocketAddrV6>().ok().or_else(|| {
		let address: Ipv6Addr = text.parse().ok()?;
		Some(SocketAddrV6::new(address, irc::PORT, 0, 0))
	})
}

/// Drawing, kept apart from the state it shows.
impl Client {
	pub fn draw(&self, frame: &mut Frame, area: Rect) {
		let client = self;
		if client.phase == Phase::Setup {
			let mut lines = Vec::new();
			for (index, label) in ["server", "nick", "join"].iter().enumerate() {
				lines.push(field_line(
					label,
					&client.form[index],
					index == client.field,
				));
			}
			if let Some(error) = &client.error {
				lines.push(Line::raw(""));
				lines.push(Line::styled(
					format!(" {error}"),
					Style::new().fg(Color::Yellow),
				));
			}
			frame.render_widget(Paragraph::new(lines), area);
			return;
		}
		let (header, rest) = split_top(area, 1);
		let (log, input) = split_bottom(rest, 1);
		let buffer = &client.buffers[client.current];
		let mut spans = Vec::from([Span::styled(
			format!(" {}", buffer.name),
			Style::new().fg(ACCENT),
		)]);
		if client.phase == Phase::Connecting {
			spans.push(Span::styled(" connecting…", DIM));
		} else {
			spans.push(Span::styled(format!(" as {}", client.nick), DIM));
		}
		for other in client.buffers.iter().filter(|other| other.unread > 0) {
			spans.push(Span::raw(format!(" {}({})", other.name, other.unread)));
		}
		frame.render_widget(Paragraph::new(Line::from(spans)), header);
		let width = usize::from(log.width.saturating_sub(1)).max(1);
		let wrapped: Vec<String> = buffer
			.lines
			.iter()
			.flat_map(|line| wrap(line, width))
			.collect();
		let height = usize::from(log.height);
		let end = wrapped
			.len()
			.saturating_sub(client.scroll.min(wrapped.len().saturating_sub(height)));
		let start = end.saturating_sub(height);
		let lines: Vec<Line> = wrapped[start..end]
			.iter()
			.map(|line| Line::raw(format!(" {line}")))
			.collect();
		frame.render_widget(Paragraph::new(lines), log);
		frame.render_widget(Paragraph::new(field_line(">", &client.input, true)), input);
	}

	pub fn hints(&self) -> &'static str {
		if self.phase == Phase::Setup {
			"↑↓ field  enter connect  esc back"
		} else {
			"tab buffer  ↑↓ scroll  esc leave"
		}
	}
}
