//! The Call screen: dialing an address, ringing, and the call itself. Voice
//! goes with push-to-talk on the board's G0 button.

use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use yggdrasil_espnow::Instant;

use super::input::TextInput;
use super::{Command, Key};
use crate::voice::iax2::CallEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
	Idle,
	Calling,
	Ringing,
	Incoming,
	Up,
}

pub enum Outcome {
	Stay,
	Leave,
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

	pub fn handle_key(&mut self, key: Key, now: Instant, commands: &mut Vec<Command>) -> Outcome {
		match (self.phase, key) {
			(Phase::Idle, Key::Enter) => match self.address.text.trim().parse() {
				Ok(address) => {
					commands.push(Command::Call(address));
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
				commands.push(Command::Answer);
				self.phase = Phase::Up;
				self.since = Some(now);
			}
			(Phase::Calling | Phase::Ringing | Phase::Incoming | Phase::Up, Key::Escape) => {
				commands.push(Command::HangUp);
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
