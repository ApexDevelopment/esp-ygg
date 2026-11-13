//! The IRC app: the text interface's client, shown in the graphical one.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use esp_ygg_app::ui::Commands;
use esp_ygg_app::ui::irc::{Client, Entry, Phase, default_nick};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::front::sync;
use crate::{ChatEntry, IrcBuffer, IrcField, Launcher, MainWindow};

/// The most lines of a conversation shown. Each is laid out whether
/// showing or not.
const SHOWN_LINES: usize = 30;

pub struct Irc {
	window: MainWindow,
	client: Client,
	lines: Rc<VecModel<ChatEntry>>,
	buffers: Rc<VecModel<IrcBuffer>>,
	/// The conversation shown, and how many lines it had then.
	shown: Option<(String, usize)>,
	/// Whether the client changed since it was last shown.
	changed: bool,
	viewing: bool,
}

impl Irc {
	pub fn new(window: &MainWindow, board_name: &str) -> Self {
		let lines = Rc::new(VecModel::default());
		let buffers = Rc::new(VecModel::default());
		let irc = window.global::<crate::Irc>();
		irc.set_lines(ModelRc::from(lines.clone()));
		irc.set_buffers(ModelRc::from(buffers.clone()));
		Irc {
			window: window.clone_strong(),
			client: Client::new(&default_nick(board_name)),
			lines,
			buffers,
			shown: None,
			changed: true,
			viewing: false,
		}
	}

	pub fn form(&self, field: IrcField) -> String {
		self.client.form[form_index(field)].text.clone()
	}

	pub fn set_form(&mut self, field: IrcField, text: &str) {
		self.client.form[form_index(field)].text = text.trim().into();
		self.changed = true;
	}

	pub fn connect(&mut self, commands: &mut Commands) {
		self.client.connect(commands);
		// The conversations start afresh.
		self.shown = None;
		self.changed = true;
	}

	/// Sends a message to the conversation showing, or runs a command.
	pub fn submit(&mut self, text: &str, commands: &mut Commands) {
		self.client.submit(text.trim(), commands);
		self.changed = true;
	}

	pub fn current(&self) -> &str {
		&self.client.buffers[self.client.current].name
	}

	pub fn switch_to(&mut self, name: &str) {
		self.client.switch_to(name);
		self.changed = true;
	}

	pub fn opened(&mut self, commands: &mut Commands) {
		self.client.opened(commands);
		self.changed = true;
	}

	pub fn received(&mut self, data: &[u8], commands: &mut Commands) {
		self.client.received(data, self.viewing, commands);
		self.changed = true;
	}

	pub fn closed(&mut self) {
		self.client.closed();
		self.changed = true;
	}

	/// Shows what changed, if anything, counting the conversation showing
	/// as read while `viewing`.
	pub fn refresh(&mut self, viewing: bool) {
		if viewing && !self.viewing {
			self.client.mark_read();
			self.changed = true;
		}
		self.viewing = viewing;
		if !self.changed {
			return;
		}
		self.changed = false;
		let client = &self.client;
		let irc = self.window.global::<crate::Irc>();
		irc.set_connected(client.phase != Phase::Setup);
		irc.set_connecting(client.phase == Phase::Connecting);
		irc.set_server(client.form[0].text.as_str().into());
		irc.set_nick(client.form[1].text.as_str().into());
		irc.set_channel(client.form[2].text.as_str().into());
		irc.set_error(client.error.as_deref().unwrap_or_default().into());
		let buffer = &client.buffers[client.current];
		irc.set_buffer(buffer.name.as_str().into());
		let others = client.unread() - buffer.unread;
		irc.set_others_unread(others as i32);
		let buffers = client
			.buffers
			.iter()
			.map(|buffer| IrcBuffer {
				name: buffer.name.as_str().into(),
				unread: buffer.unread as i32,
			})
			.collect();
		sync(&self.buffers, buffers);
		self.window
			.global::<Launcher>()
			.set_irc_unread(client.unread() as i32);
		self.show_lines();
	}

	/// Adds the lines that arrived in the conversation showing, newest
	/// first, or shows another conversation afresh.
	fn show_lines(&mut self) {
		let buffer = &self.client.buffers[self.client.current];
		let nick = &self.client.nick;
		let added = match &self.shown {
			Some((name, shown)) if *name == buffer.name => {
				buffer.added.checked_sub(*shown).unwrap_or(usize::MAX)
			}
			_ => usize::MAX,
		};
		if added < SHOWN_LINES {
			let skipped = buffer.lines.len().saturating_sub(added);
			for entry in buffer.lines.iter().skip(skipped) {
				self.lines.insert(0, chat_entry(entry, nick));
			}
			while self.lines.row_count() > SHOWN_LINES {
				self.lines.remove(self.lines.row_count() - 1);
			}
		} else if added > 0 {
			let rows: Vec<ChatEntry> = buffer
				.lines
				.iter()
				.rev()
				.take(SHOWN_LINES)
				.map(|entry| chat_entry(entry, nick))
				.collect();
			self.lines.set_vec(rows);
		}
		self.shown = Some((buffer.name.clone(), buffer.added));
	}
}

fn form_index(field: IrcField) -> usize {
	match field {
		IrcField::Server => 0,
		IrcField::Nick => 1,
		IrcField::Channel => 2,
	}
}

fn chat_entry(entry: &Entry, nick: &str) -> ChatEntry {
	let from = entry.from.as_deref().unwrap_or_default();
	ChatEntry {
		from: from.to_string().into(),
		text: entry.text.as_str().into(),
		mine: from == nick,
	}
}
