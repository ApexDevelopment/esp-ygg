//! A Gopher browser: an address line, menus to pick from, text to scroll,
//! and history to go back through.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::view::{DIM, SELECTED, field_line, visible_end};

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};

use super::{Command, Commands, Key, Outcome, TextInput, navigation};
use crate::gopher::{self, Item, Url};

const MAX_HISTORY: usize = 16;

pub(super) enum Content {
	Menu { items: Vec<Item>, selected: usize },
	Text { text: String, scroll: u16 },
}

pub(super) struct Page {
	pub url: Url,
	pub content: Content,
}

struct Pending {
	url: Url,
	address: Ipv6Addr,
}

pub(super) struct Browser {
	pub address: TextInput,
	pub editing: bool,
	pub page: Option<Page>,
	history: Vec<Page>,
	pending: Option<Pending>,
	/// A search waiting for its query.
	pub search: Option<(Url, Ipv6Addr, TextInput)>,
	pub error: Option<String>,
	/// Host names learned to belong to addresses, in lowercase.
	aliases: BTreeMap<String, Ipv6Addr>,
}

impl Browser {
	pub fn new() -> Self {
		Browser {
			address: TextInput::new("", 60),
			editing: true,
			page: None,
			history: Vec::new(),
			pending: None,
			search: None,
			error: None,
			aliases: BTreeMap::new(),
		}
	}

	pub fn is_loading(&self) -> bool {
		self.pending.is_some()
	}

	pub fn handle_key(&mut self, key: Key, commands: &mut Commands) -> Outcome {
		if let Some((url, address, query)) = &mut self.search {
			match key {
				Key::Enter => {
					let (url, address, query) = (url.clone(), *address, query.text.clone());
					self.search = None;
					self.request(url, address, Some(&query), commands);
				}
				Key::Escape => self.search = None,
				Key::Backspace => query.backspace(),
				Key::Char(character) => query.push(character),
				_ => {}
			}
			return Outcome::Stay;
		}
		if self.editing {
			match key {
				Key::Enter => self.go_to_address(commands),
				Key::Escape if self.page.is_some() => self.editing = false,
				Key::Escape => return Outcome::Leave,
				Key::Backspace => self.address.backspace(),
				Key::Char(character) => self.address.push(character),
				_ => {}
			}
			return Outcome::Stay;
		}
		match navigation(key) {
			Some(Key::Char('g')) => {
				let current = self.page.as_ref().map(|page| page.url.to_string());
				self.address = TextInput::prefilled(&current.unwrap_or_default(), 60);
				self.editing = true;
			}
			Some(Key::Escape | Key::Left) => match self.history.pop() {
				Some(previous) => self.page = Some(previous),
				None => return Outcome::Leave,
			},
			Some(Key::Up) => self.move_by(-1),
			Some(Key::Down) => self.move_by(1),
			Some(Key::Enter | Key::Right) => self.open_selected(commands),
			_ => {}
		}
		Outcome::Stay
	}

	fn go_to_address(&mut self, commands: &mut Commands) {
		let Some(url) = Url::parse(&self.address.text) else {
			self.error = Some("Not a Gopher address".into());
			return;
		};
		match self.resolve(&url) {
			Some(address) => {
				self.editing = false;
				self.request(url, address, None, commands);
			}
			None => self.error = Some(cannot_resolve(&url)),
		}
	}

	fn open_selected(&mut self, commands: &mut Commands) {
		let Some(page) = &self.page else {
			return;
		};
		let Content::Menu { items, selected } = &page.content else {
			return;
		};
		let Some(item) = items.get(*selected).filter(|item| item.is_link()) else {
			return;
		};
		if !item.is_supported() {
			self.error = Some(format!("Can't open items of type {}", item.kind));
			return;
		}
		let url = item.url();
		let Some(address) = self.resolve(&url) else {
			self.error = Some(cannot_resolve(&url));
			return;
		};
		if url.kind == '7' {
			self.search = Some((url, address, TextInput::new("", 60)));
		} else {
			self.request(url, address, None, commands);
		}
	}

	fn request(
		&mut self,
		url: Url,
		address: Ipv6Addr,
		query: Option<&str>,
		commands: &mut Commands,
	) {
		self.error = None;
		let to = SocketAddrV6::new(address, url.port, 0, 0);
		commands.push_back(Command::Fetch(to, url.request(query)));
		self.pending = Some(Pending { url, address });
	}

	pub fn fetched(&mut self, data: &[u8]) {
		let Some(Pending { url, address }) = self.pending.take() else {
			return;
		};
		let content = match url.kind {
			'0' => Content::Text {
				text: gopher::parse_text(data),
				scroll: 0,
			},
			_ => {
				let items = gopher::parse_menu(data);
				self.learn_alias(&items, address);
				let selected = items.iter().position(Item::is_link).unwrap_or(0);
				Content::Menu { items, selected }
			}
		};
		if let Some(previous) = self.page.take() {
			if self.history.len() == MAX_HISTORY {
				self.history.remove(0);
			}
			self.history.push(previous);
		}
		self.page = Some(Page { url, content });
	}

	pub fn failed(&mut self) {
		if let Some(pending) = self.pending.take() {
			self.error = Some(format!("Could not reach {}", pending.address));
		}
	}

	/// Moves the selection to the next link, or scrolls text.
	fn move_by(&mut self, step: i32) {
		let Some(page) = &mut self.page else {
			return;
		};
		match &mut page.content {
			Content::Text { scroll, .. } => {
				*scroll = scroll.saturating_add_signed(step as i16);
			}
			Content::Menu { items, selected } => {
				let mut index = *selected as i32;
				loop {
					index += step;
					let Some(item) = usize::try_from(index)
						.ok()
						.and_then(|index| items.get(index))
					else {
						// No more links that way, so show the lines past the last one.
						if items.iter().all(|item| !item.is_link()) {
							*selected =
								(*selected as i32 + step).clamp(0, items.len() as i32 - 1) as usize;
						}
						return;
					};
					if item.is_link() {
						*selected = index as usize;
						return;
					}
				}
			}
		}
	}
}

impl Browser {
	/// The host's address, or the address its name was learned to have.
	fn resolve(&self, url: &Url) -> Option<Ipv6Addr> {
		url.address()
			.or_else(|| self.aliases.get(&url.host.to_ascii_lowercase()).copied())
	}

	/// Menus mostly link to their own server by its name, so the name most
	/// of a menu's links use is taken to be that of the address it came from.
	fn learn_alias(&mut self, items: &[Item], address: Ipv6Addr) {
		let mut counts: BTreeMap<String, usize> = BTreeMap::new();
		for item in items
			.iter()
			.filter(|item| item.is_link() && item.url().address().is_none())
		{
			*counts.entry(item.host.to_ascii_lowercase()).or_default() += 1;
		}
		if let Some((name, _)) = counts.into_iter().max_by_key(|(_, count)| *count)
			&& !name.is_empty()
		{
			self.aliases.insert(name, address);
		}
	}
}

fn cannot_resolve(url: &Url) -> String {
	format!("Can't look up {}; use an address", url.host)
}

/// Drawing, kept apart from the state it shows.
impl Browser {
	pub fn draw(&self, frame: &mut Frame, area: Rect) {
		let browser = self;
		let [top, status, body] = Layout::vertical([
			Constraint::Length(1),
			Constraint::Length(1),
			Constraint::Fill(1),
		])
		.areas(area);
		let top_line = match (&browser.search, &browser.page) {
			(Some((_, _, query)), _) => field_line("search", query, true),
			(None, Some(page)) if !browser.editing => {
				Line::styled(format!(" {}", visible_end(&page.url.to_string(), 38)), DIM)
			}
			_ => field_line("go", &browser.address, browser.editing),
		};
		frame.render_widget(Paragraph::new(top_line), top);
		let status_line = if let Some(error) = &browser.error {
			Line::styled(format!(" {error}"), Style::new().fg(Color::Yellow))
		} else if browser.is_loading() {
			Line::styled(" Loading…", DIM)
		} else {
			Line::raw("")
		};
		frame.render_widget(Paragraph::new(status_line), status);
		let Some(page) = &browser.page else {
			return;
		};
		match &page.content {
			Content::Text { text, scroll } => {
				let text = Paragraph::new(text.as_str())
					.wrap(Wrap { trim: false })
					.scroll((*scroll, 0));
				frame.render_widget(text, body);
			}
			Content::Menu { items, selected } => {
				let lines: Vec<ListItem> = items
					.iter()
					.map(|item| {
						let text = match item.kind {
							'1' => format!("{}/", item.text),
							'7' => format!("{}?", item.text),
							_ => item.text.clone(),
						};
						let style = if item.is_link() && item.is_supported() {
							Style::new()
						} else {
							DIM
						};
						ListItem::new(Line::styled(text, style))
					})
					.collect();
				let list = List::new(lines).highlight_style(SELECTED);
				let mut state = ListState::default().with_selected(Some(*selected));
				frame.render_stateful_widget(list, body, &mut state);
			}
		}
	}

	pub fn hints(&self) -> &'static str {
		if self.search.is_some() {
			"type a search  enter go"
		} else if self.editing {
			"tab complete  enter go"
		} else if let Some(Page {
			content: Content::Text { .. },
			..
		}) = &self.page
		{
			"↑↓ scroll  g address  esc back"
		} else {
			"enter open  g address  esc back"
		}
	}
}
