//! The UDP console: messages typed to one node, and those sent to the
//! console's port.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::net::SocketAddrV6;

use esp_ygg_app::CONSOLE_PORT;
use esp_ygg_app::ui::Command;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{ConsoleLine, Launcher, MainWindow};

/// The most messages kept. Each is laid out whether showing or not.
const LOG_LINES: usize = 30;

pub struct Console {
	window: MainWindow,
	target: Option<SocketAddrV6>,
	log: Rc<VecModel<ConsoleLine>>,
	unread: i32,
}

impl Console {
	pub fn new(window: &MainWindow) -> Self {
		let log = Rc::new(VecModel::default());
		let console = window.global::<crate::Console>();
		console.set_log(ModelRc::from(log.clone()));
		console.set_port(CONSOLE_PORT.into());
		Console {
			window: window.clone_strong(),
			target: None,
			log,
			unread: 0,
		}
	}

	pub fn target(&self) -> Option<SocketAddrV6> {
		self.target
	}

	/// Sends messages to `to`, shown as `label`, from now on.
	pub fn set_target(&mut self, to: SocketAddrV6, label: String) {
		self.target = Some(to);
		self.global().set_target(label.into());
	}

	/// Sends `text` to the node chosen, if one is.
	pub fn send(&mut self, text: &str) -> Option<Command> {
		let to = self.target?;
		if text.is_empty() {
			return None;
		}
		let label = self.global().get_target();
		self.add_line(ConsoleLine {
			from: format!("To {label}").into(),
			text: text.into(),
			mine: true,
		});
		let mut data = String::from(text).into_bytes();
		data.push(b'\n');
		Some(Command::SendDatagram(to, data))
	}

	/// Logs a message from `sender`, shown as `label`, counting it as unread
	/// unless `viewing`. Replies go to the first sender until a node is
	/// chosen.
	pub fn received(&mut self, sender: SocketAddrV6, label: String, data: &[u8], viewing: bool) {
		if self.target.is_none() {
			let reply_to = SocketAddrV6::new(*sender.ip(), CONSOLE_PORT, 0, 0);
			self.set_target(reply_to, label.clone());
		}
		let text = String::from_utf8_lossy(data);
		self.add_line(ConsoleLine {
			from: label.into(),
			text: text.trim_end().into(),
			mine: false,
		});
		if !viewing {
			self.unread += 1;
			self.window
				.global::<Launcher>()
				.set_console_unread(self.unread);
		}
	}

	/// Returns whether any messages were unread.
	pub fn mark_read(&mut self) -> bool {
		if self.unread == 0 {
			return false;
		}
		self.unread = 0;
		self.window.global::<Launcher>().set_console_unread(0);
		true
	}

	fn add_line(&self, line: ConsoleLine) {
		self.log.insert(0, line);
		if self.log.row_count() > LOG_LINES {
			self.log.remove(LOG_LINES);
		}
	}

	fn global(&self) -> crate::Console<'_> {
		self.window.global::<crate::Console>()
	}
}
