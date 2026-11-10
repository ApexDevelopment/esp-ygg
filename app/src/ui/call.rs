//! The Call screen: dialing an address, ringing, and the call itself. Voice
//! goes with push-to-talk on the board's G0 button.

use alloc::format;
use alloc::vec::Vec;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::view::{DIM, SELECTED, field_line, pad};

use alloc::string::String;
use core::net::Ipv6Addr;

use yggdrasil_espnow::Instant;

use super::input::TextInput;
use super::{Command, Commands, Key, Outcome};
use crate::voice::iax2::CallEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
	Idle,
	Calling,
	Ringing,
	Incoming,
	Up,
}

pub struct Phone {
	pub address: TextInput,
	pub phase: Phase,
	/// Who the call is with, and the name they gave, if any.
	pub peer: Option<(Ipv6Addr, Option<String>)>,
	pub since: Option<Instant>,
	/// Set while push-to-talk is held.
	pub talking: bool,
	/// Why the last call ended.
	pub ended: Option<String>,
}

impl Phone {
	pub fn new() -> Self {
		Phone {
			address: TextInput::new("", 39),
			phase: Phase::Idle,
			peer: None,
			since: None,
			talking: false,
			ended: None,
		}
	}

	pub fn handle_key(&mut self, key: Key, now: Instant, commands: &mut Commands) -> Outcome {
		match (self.phase, key) {
			(Phase::Idle, Key::Enter) => match self.address.text.trim().parse() {
				Ok(address) => {
					commands.push_back(Command::Call(address));
					self.phase = Phase::Calling;
					self.peer = Some((address, None));
					self.ended = None;
				}
				Err(_) => self.ended = Some("Enter an address to call".into()),
			},
			(Phase::Idle, Key::Escape | Key::Left) => return Outcome::Leave,
			(Phase::Idle, Key::Backspace) => self.address.backspace(),
			(Phase::Idle, Key::Char(character)) => self.address.push(character),
			(Phase::Incoming, Key::Enter) => {
				commands.push_back(Command::Answer);
				self.phase = Phase::Up;
				self.since = Some(now);
			}
			(Phase::Calling | Phase::Ringing | Phase::Incoming | Phase::Up, Key::Escape) => {
				commands.push_back(Command::HangUp);
				self.phase = Phase::Idle;
				self.ended = Some("Hung up".into());
			}
			_ => {}
		}
		Outcome::Stay
	}

	/// Returns whether the screen should come up, for an incoming call.
	pub fn handle_event(&mut self, event: CallEvent, now: Instant) -> bool {
		match event {
			CallEvent::Incoming { from, name } => {
				self.phase = Phase::Incoming;
				self.peer = Some((from, name));
				self.ended = None;
				return true;
			}
			CallEvent::Ringing if self.phase == Phase::Calling => self.phase = Phase::Ringing,
			CallEvent::Answered => {
				self.phase = Phase::Up;
				self.since = Some(now);
			}
			CallEvent::Ended(reason) => {
				// Hanging up here already said so.
				if self.phase != Phase::Idle {
					self.ended = Some(reason);
				}
				self.phase = Phase::Idle;
				self.since = None;
			}
			CallEvent::Ringing => {}
		}
		false
	}
}

/// Drawing, kept apart from the state it shows.
impl Phone {
	pub fn draw(&self, frame: &mut Frame, area: Rect, now: Instant, who: &str) {
		let phone = self;
		let mut lines = Vec::new();
		match phone.phase {
			Phase::Idle => {
				lines.push(field_line("to", &phone.address, true));
				lines.push(Line::raw(""));
				if let Some(ended) = &phone.ended {
					lines.push(Line::styled(format!(" {ended}"), DIM));
				}
				lines.push(Line::styled(" Hold G0 to hear yourself back", DIM));
			}
			Phase::Calling => lines.push(Line::raw(format!(" Calling {who}…"))),
			Phase::Ringing => lines.push(Line::raw(format!(" Ringing {who}…"))),
			Phase::Incoming => {
				lines.push(Line::styled(" Incoming call from", DIM));
				lines.push(Line::raw(format!(" {who}")));
			}
			Phase::Up => {
				lines.push(Line::raw(format!(" In a call with {who}")));
				if let Some(since) = phone.since {
					let seconds = now.saturating_duration_since(since).as_secs();
					lines.push(Line::styled(
						format!(" {}:{:02}", seconds / 60, seconds % 60),
						DIM,
					));
				}
				lines.push(Line::raw(""));
				lines.push(if phone.talking {
					Line::styled(pad(" Talking", area.width), SELECTED)
				} else {
					Line::styled(" Hold G0 to talk", DIM)
				});
			}
		}
		frame.render_widget(Paragraph::new(lines), area);
	}

	pub fn hints(&self) -> &'static str {
		match self.phase {
			Phase::Idle => "tab complete  enter call",
			Phase::Incoming => "enter answer  esc decline",
			Phase::Calling | Phase::Ringing | Phase::Up => "esc hang up",
		}
	}
}
