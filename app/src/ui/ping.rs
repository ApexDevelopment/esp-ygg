//! The Ping screen: a ping every second to one address, with each round trip.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use core::net::Ipv6Addr;
use core::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use yggdrasil_espnow::Instant;

use crate::ping::Session;

use super::view::{DIM, field_line, split_top, tail};
use super::{Command, Commands, Key, Outcome, TextInput, push_limited};

const INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct Ping {
	pub target: TextInput,
	pub running: bool,
	last: Option<Instant>,
	log: VecDeque<String>,
	session: Session,
}

impl Ping {
	pub fn new() -> Self {
		Ping {
			target: TextInput::new("", 45),
			running: false,
			last: None,
			log: VecDeque::new(),
			session: Session::default(),
		}
	}

	/// Starts pinging `address` afresh.
	pub fn start_at(&mut self, address: Ipv6Addr) {
		self.target = TextInput::new(&address.to_string(), 45);
		self.start();
	}

	fn start(&mut self) {
		self.running = true;
		self.last = None;
		self.log.clear();
		if let Ok(target) = self.target.text.parse() {
			self.session.start(target);
		}
	}

	pub fn handle_key(&mut self, key: Key) -> Outcome {
		match key {
			Key::Enter if self.running => self.running = false,
			Key::Enter if self.target.text.parse::<Ipv6Addr>().is_ok() => self.start(),
			Key::Enter => return Outcome::Notice("Not an IPv6 address"),
			Key::Escape => {
				self.running = false;
				return Outcome::Leave;
			}
			Key::Backspace if !self.running => self.target.backspace(),
			Key::Char(character) if !self.running => self.target.push(character),
			_ => {}
		}
		Outcome::Stay
	}

	pub fn tick(&mut self, now: Instant, commands: &mut Commands) {
		if self.running
			&& self.last.is_none_or(|last| now >= last + INTERVAL)
			&& let Ok(target) = self.target.text.parse()
		{
			commands.push_back(Command::Ping(target));
			self.last = Some(now);
			self.session.record_sent();
		}
	}

	pub fn replied(&mut self, from: Ipv6Addr, sequence: u16, round_trip: Duration) {
		if self.session.replied(from, round_trip) {
			push_limited(
				&mut self.log,
				format!("#{sequence}  {} ms", round_trip.as_millis()),
			);
		}
	}

	pub fn timed_out(&mut self, target: Ipv6Addr, sequence: u16) {
		if self.session.target == Some(target) {
			push_limited(&mut self.log, format!("#{sequence}  timed out"));
		}
	}

	pub fn draw(&self, frame: &mut Frame, area: Rect) {
		let (target, rest) = split_top(area, 1);
		let (summary, log) = split_top(rest, 1);
		frame.render_widget(
			Paragraph::new(field_line("to", &self.target, !self.running)),
			target,
		);
		let mut text = format!(
			" sent {}  received {}",
			self.session.sent, self.session.received
		);
		if let Some(average) = self.session.average() {
			text.push_str(&format!("  avg {} ms", average.as_millis()));
		}
		frame.render_widget(Paragraph::new(Line::styled(text, DIM)), summary);
		frame.render_widget(Paragraph::new(tail(&self.log, log.height)), log);
	}

	pub fn hints(&self) -> &'static str {
		if self.running {
			"enter stop  esc back"
		} else {
			"tab complete  enter start"
		}
	}
}
