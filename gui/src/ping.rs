//! The Ping app: a ping a second to one node, with each round trip.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::net::Ipv6Addr;
use core::time::Duration;

use esp_ygg_app::ui::Command;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::MainWindow;

const INTERVAL: Duration = Duration::from_secs(1);
/// The most round trips kept in the log.
const LOG_LINES: usize = 50;

pub struct Pinger {
	window: MainWindow,
	target: Option<Ipv6Addr>,
	/// Asks for the next ping each interval while running.
	timer: Timer,
	sent: u32,
	received: u32,
	total: Duration,
	log: Rc<VecModel<SharedString>>,
}

impl Pinger {
	/// Calls `tick` every interval while running, to ask for a ping.
	pub fn new(window: &MainWindow, tick: impl FnMut() + 'static) -> Self {
		let timer = Timer::default();
		timer.start(TimerMode::Repeated, INTERVAL, tick);
		timer.stop();
		let log = Rc::new(VecModel::default());
		window
			.global::<crate::Ping>()
			.set_log(ModelRc::from(log.clone()));
		Pinger {
			window: window.clone_strong(),
			target: None,
			timer,
			sent: 0,
			received: 0,
			total: Duration::ZERO,
			log,
		}
	}

	pub fn target(&self) -> Option<Ipv6Addr> {
		self.target
	}

	/// Pings `address`, shown as `label`, from now on.
	pub fn set_target(&mut self, address: Ipv6Addr, label: String) -> Option<Command> {
		self.target = Some(address);
		self.global().set_target(label.into());
		self.start()
	}

	/// Starts afresh, with the first ping.
	pub fn start(&mut self) -> Option<Command> {
		self.sent = 0;
		self.received = 0;
		self.total = Duration::ZERO;
		self.log.clear();
		self.show_summary();
		self.global().set_running(true);
		self.timer.restart();
		self.send()
	}

	pub fn stop(&mut self) {
		self.timer.stop();
		self.global().set_running(false);
	}

	/// The next ping, if running.
	pub fn send(&mut self) -> Option<Command> {
		if !self.global().get_running() {
			return None;
		}
		let target = self.target?;
		self.sent += 1;
		self.show_summary();
		Some(Command::Ping(target))
	}

	pub fn replied(&mut self, from: Ipv6Addr, sequence: u16, round_trip: Duration) {
		if self.target != Some(from) {
			return;
		}
		self.received += 1;
		self.total += round_trip;
		self.add_line(format!("#{sequence}  {} ms", round_trip.as_millis()));
		self.show_summary();
	}

	pub fn timed_out(&mut self, target: Ipv6Addr, sequence: u16) {
		if self.target == Some(target) {
			self.add_line(format!("#{sequence}  timed out"));
		}
	}

	fn add_line(&self, line: String) {
		self.log.insert(0, line.into());
		if self.log.row_count() > LOG_LINES {
			self.log.remove(LOG_LINES);
		}
	}

	fn show_summary(&self) {
		let mut summary = format!("{} of {} answered", self.received, self.sent);
		if self.received > 0 {
			let average = self.total / self.received;
			summary.push_str(&format!(" · {} ms", average.as_millis()));
		}
		self.global().set_summary(summary.into());
	}

	fn global(&self) -> crate::Ping<'_> {
		self.window.global::<crate::Ping>()
	}
}
