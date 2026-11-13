//! The Share screen: the shared folder's files, sending one, and what Share
//! asks of the person: whether to let a peer in, and a server's password.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::view::{DIM, SELECTED, field_line, pad, split_top, truncate, wrap};
use super::{Command, Commands, Key, Outcome, TextInput, navigation};
use crate::share::{Entry, Status, size_text};

const USER_LEN: usize = 32;
const PASSWORD_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
	Files,
	/// Typing where to send the chosen file.
	Address,
	/// Typing who to log in as there.
	User,
	/// Typing the password a server asked for.
	Password,
}

pub(super) struct Share {
	files: Vec<Entry>,
	selected: usize,
	pub phase: Phase,
	pub address: TextInput,
	user: TextInput,
	password: TextInput,
	status: Status,
}

impl Share {
	pub fn new() -> Self {
		Share {
			files: Vec::new(),
			selected: 0,
			phase: Phase::Files,
			address: TextInput::new("", 45),
			user: TextInput::new("share", USER_LEN),
			password: TextInput::new("", PASSWORD_LEN),
			status: Status::default(),
		}
	}

	pub fn opened(&mut self, commands: &mut Commands) {
		commands.push_back(Command::ShareList);
	}

	pub fn set_files(&mut self, mut files: Vec<Entry>) {
		files.sort_by_key(|entry| entry.name.to_lowercase());
		self.selected = self.selected.min(files.len().saturating_sub(1));
		self.files = files;
	}

	/// Takes what Share is doing. Returns whether it newly asks something
	/// of the person, so the screen should come up.
	pub fn set_status(&mut self, status: Status, commands: &mut Commands) -> bool {
		if status.ended_since(&self.status) {
			commands.push_back(Command::ShareList);
		}
		let newly_asking = status.asking.is_some() && status.asking != self.status.asking;
		let wants_password = status.password_for.is_some() && self.status.password_for.is_none();
		if wants_password {
			self.phase = Phase::Password;
			self.password = TextInput::new("", PASSWORD_LEN);
			self.password.secret = true;
		} else if status.password_for.is_none() && self.phase == Phase::Password {
			self.phase = Phase::Files;
		}
		self.status = status;
		newly_asking || wants_password
	}

	pub fn is_asking(&self) -> bool {
		self.status.asking.is_some()
	}

	/// Whether keys go into a text field.
	pub fn typing(&self) -> bool {
		self.phase != Phase::Files && !self.is_asking()
	}

	pub fn handle_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		if self.is_asking() {
			let admit = match navigation(key) {
				Some(Key::Enter | Key::Char('y')) => true,
				Some(Key::Escape | Key::Char('n')) => false,
				_ => return Outcome::Stay,
			};
			commands.push_back(Command::ShareAdmit(admit));
			self.status.asking = None;
			return Outcome::Stay;
		}
		match self.phase {
			Phase::Files => return self.files_key(key),
			Phase::Address => match key {
				Key::Enter if self.address.text.parse::<Ipv6Addr>().is_ok() => {
					self.phase = Phase::User
				}
				Key::Enter => return Outcome::Notice("Not an IPv6 address"),
				Key::Escape => self.phase = Phase::Files,
				_ => edit(&mut self.address, key),
			},
			Phase::User => match key {
				Key::Enter => self.send(commands),
				Key::Escape => self.phase = Phase::Address,
				_ => edit(&mut self.user, key),
			},
			Phase::Password => match key {
				Key::Enter | Key::Escape => {
					let password = (key == Key::Enter).then(|| self.password.text.clone());
					commands.push_back(Command::SharePassword(password));
					self.status.password_for = None;
					self.phase = Phase::Files;
				}
				_ => edit(&mut self.password, key),
			},
		}
		Outcome::Stay
	}

	fn files_key(&mut self, key: Key) -> Outcome {
		match navigation(key) {
			Some(Key::Up) => self.selected = self.selected.saturating_sub(1),
			Some(Key::Down) => {
				self.selected = (self.selected + 1).min(self.files.len().saturating_sub(1))
			}
			Some(Key::Enter | Key::Right) if !self.files.is_empty() => self.phase = Phase::Address,
			Some(Key::Escape | Key::Left) => return Outcome::Leave,
			_ => {}
		}
		Outcome::Stay
	}

	fn send(&mut self, commands: &mut Commands) {
		let (Ok(to), Some(file)) = (self.address.text.parse(), self.files.get(self.selected))
		else {
			return;
		};
		commands.push_back(Command::ShareSend {
			to,
			user: self.user.text.clone(),
			name: file.name.clone(),
		});
		self.phase = Phase::Files;
	}

	/// Draws the screen, naming a peer that asks to come in with `label`.
	pub fn draw(&self, frame: &mut Frame, area: Rect, label: impl Fn(&Ipv6Addr) -> String) {
		if let Some(asking) = &self.status.asking {
			let mut lines = Vec::from([Line::raw(" Share files with")]);
			let width = usize::from(area.width.saturating_sub(1));
			for line in wrap(&label(asking), width) {
				lines.push(Line::styled(format!(" {line}"), DIM));
			}
			frame.render_widget(Paragraph::new(lines), area);
			return;
		}
		let summary = self.status.summary();
		let area = if summary.is_empty() {
			area
		} else {
			let (top, rest) = split_top(area, 1);
			frame.render_widget(
				Paragraph::new(Line::styled(format!(" {summary}"), DIM)),
				top,
			);
			rest
		};
		let name = self.files.get(self.selected).map(|file| file.name.as_str());
		let lines = match self.phase {
			Phase::Files => self.file_lines(area),
			Phase::Address => Vec::from([
				Line::styled(format!(" Send {}", name.unwrap_or_default()), DIM),
				field_line("to", &self.address, true),
			]),
			Phase::User => Vec::from([
				Line::styled(format!(" Send {}", name.unwrap_or_default()), DIM),
				field_line("to", &self.address, false),
				field_line("user", &self.user, true),
			]),
			Phase::Password => {
				let user = self.status.password_for.as_deref().unwrap_or_default();
				Vec::from([
					Line::styled(format!(" Password for {user}"), DIM),
					field_line("password", &self.password, true),
				])
			}
		};
		frame.render_widget(Paragraph::new(lines), area);
	}

	fn file_lines(&self, area: Rect) -> Vec<Line<'static>> {
		if self.files.is_empty() {
			return Vec::from([Line::styled(" No files yet", DIM)]);
		}
		// Keep the selection on screen when the list is longer than the space.
		let skip = (self.selected + 1).saturating_sub(usize::from(area.height));
		self.files
			.iter()
			.enumerate()
			.skip(skip)
			.map(|(index, file)| {
				let size = size_text(file.size);
				let room = area.width.saturating_sub(size.len() as u16 + 2);
				let name = truncate(&file.name, usize::from(room.saturating_sub(1).max(1)));
				let text = pad(&format!(" {name}"), room) + &size + " ";
				let style = if index == self.selected {
					SELECTED
				} else {
					Style::new()
				};
				Line::styled(text, style)
			})
			.collect()
	}

	pub fn hints(&self) -> &'static str {
		if self.is_asking() {
			return "enter accept  esc decline";
		}
		match self.phase {
			Phase::Files if self.files.is_empty() => "esc back",
			Phase::Files => "enter send  esc back",
			Phase::Address => "tab complete  enter next",
			Phase::User => "enter send  esc back",
			Phase::Password => "enter log in  esc cancel",
		}
	}
}

fn edit(input: &mut TextInput, key: Key) {
	match key {
		Key::Backspace => input.backspace(),
		Key::Char(character) => input.push(character),
		_ => {}
	}
}
