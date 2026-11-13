//! The UDP Console: text received on the console port, and messages typed to
//! an address.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::view::{DIM, field_line, split_bottom, tail};
use super::{Command, Commands, Key, Outcome, TextInput, push_limited};
use crate::overlay::CONSOLE_PORT;

const MAX_MESSAGE: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
	Target,
	Message,
}

pub(super) struct Console {
	pub target: TextInput,
	message: TextInput,
	pub focus: Focus,
	log: VecDeque<String>,
	/// Lines received while the screen was not open.
	pub unread: usize,
}

impl Console {
	pub fn new() -> Self {
		Console {
			target: TextInput::new("", 52),
			message: TextInput::new("", MAX_MESSAGE),
			focus: Focus::Target,
			log: VecDeque::new(),
			unread: 0,
		}
	}

	/// Opens with `address` filled in, ready for a message.
	pub fn write_to(&mut self, address: Ipv6Addr) {
		self.target = TextInput::new(&address.to_string(), 52);
		self.focus = Focus::Message;
	}

	pub fn handle_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		let field = match self.focus {
			Focus::Target => &mut self.target,
			Focus::Message => &mut self.message,
		};
		match key {
			Key::Escape => return Outcome::Leave,
			Key::Tab | Key::Up | Key::Down => {
				self.focus = match self.focus {
					Focus::Target => Focus::Message,
					Focus::Message => Focus::Target,
				}
			}
			Key::Enter => match self.focus {
				Focus::Target => self.focus = Focus::Message,
				Focus::Message => return self.send(commands),
			},
			Key::Backspace => field.backspace(),
			Key::Char(character) => field.push(character),
			_ => {}
		}
		Outcome::Stay
	}

	fn send(&mut self, commands: &mut Commands) -> Outcome {
		let Some(to) = parse_destination(&self.target.text) else {
			self.focus = Focus::Target;
			return Outcome::Notice("Enter an address, or [address]:port");
		};
		if !self.message.text.is_empty() {
			let mut data = self.message.text.clone().into_bytes();
			data.push(b'\n');
			commands.push_back(Command::SendDatagram(to, data));
			push_limited(&mut self.log, format!("> {}", self.message.text));
			self.message.clear();
		}
		Outcome::Stay
	}

	/// Logs text from `sender`, counting it as unread unless `viewing`.
	pub fn received(&mut self, sender: &str, data: &[u8], viewing: bool) {
		let text = String::from_utf8_lossy(data);
		push_limited(&mut self.log, format!("{sender} {}", text.trim_end()));
		if !viewing {
			self.unread += 1;
		}
	}

	pub fn draw(&self, frame: &mut Frame, area: Rect) {
		let (rest, message) = split_bottom(area, 1);
		let (log, target) = split_bottom(rest, 1);
		let lines = if self.log.is_empty() {
			Vec::from([Line::styled(
				format!(" Listening on UDP port {CONSOLE_PORT}"),
				DIM,
			)])
		} else {
			tail(&self.log, log.height)
		};
		frame.render_widget(Paragraph::new(lines), log);
		frame.render_widget(
			Paragraph::new(field_line("to", &self.target, self.focus == Focus::Target)),
			target,
		);
		frame.render_widget(
			Paragraph::new(field_line(">", &self.message, self.focus == Focus::Message)),
			message,
		);
	}

	pub fn hints(&self) -> &'static str {
		"tab switch field  enter send"
	}
}

/// An address and port, or an address alone for the console port.
pub fn parse_destination(text: &str) -> Option<SocketAddrV6> {
	text.parse::<SocketAddrV6>().ok().or_else(|| {
		let address: Ipv6Addr = text.parse().ok()?;
		Some(SocketAddrV6::new(address, CONSOLE_PORT, 0, 0))
	})
}
