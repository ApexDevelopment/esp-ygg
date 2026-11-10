//! A Gemini browser: an address line, pages to read and follow links on,
//! questions servers ask, and history to go back through.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::net::SocketAddrV6;

use super::view::wrap;
use super::{Command, Key, TextInput, navigation};
use crate::gemini::{self, Header, Line, LinkError, Url};
use crate::overlay::Stream;
use crate::tls::{self, Fingerprint, Session};

/// The part of the screen a page fills, in characters.
pub(super) const COLUMNS: usize = 40;
pub(super) const ROWS: usize = 9;
const MAX_HISTORY: usize = 16;
const MAX_REDIRECTS: u8 = 5;
/// Responses are cut off past this, to save memory.
const MAX_RESPONSE: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowKind {
	Text,
	Heading,
	/// Part of the link with this index.
	Link(usize),
	Quote,
	Preformatted,
}

pub(super) struct Row {
	pub text: String,
	pub kind: RowKind,
}

pub(super) struct Page {
	pub url: Url,
	/// The page laid out to the screen's width.
	pub rows: Vec<Row>,
	links: Vec<String>,
	/// The first row of each link.
	link_rows: Vec<usize>,
	pub selected: Option<usize>,
	pub scroll: usize,
}

impl Page {
	fn new(url: Url, rows: Vec<Row>, links: Vec<String>) -> Self {
		let mut link_rows = Vec::new();
		for (index, row) in rows.iter().enumerate() {
			if let RowKind::Link(link) = row.kind
				&& link_rows.len() == link
			{
				link_rows.push(index);
			}
		}
		let mut page = Page {
			url,
			rows,
			links,
			link_rows,
			selected: None,
			scroll: 0,
		};
		page.selected = page.first_visible_link();
		page
	}

	fn is_visible(&self, link: usize) -> bool {
		self.link_rows
			.get(link)
			.is_some_and(|&row| (self.scroll..self.scroll + ROWS).contains(&row))
	}

	fn first_visible_link(&self) -> Option<usize> {
		(0..self.links.len()).find(|&link| self.is_visible(link))
	}

	/// The nearest link on screen past the selection, going down or up.
	fn next_visible_link(&self, down: bool) -> Option<usize> {
		let beyond = |link: usize| match self.selected {
			Some(selected) if down => link > selected,
			Some(selected) => link < selected,
			None => true,
		};
		let mut visible =
			(0..self.links.len()).filter(|&link| self.is_visible(link) && beyond(link));
		if down {
			visible.next()
		} else {
			visible.next_back()
		}
	}

	/// Selects the next link on screen, or else scrolls a screen.
	fn move_by(&mut self, down: bool) {
		if let Some(link) = self.next_visible_link(down) {
			self.selected = Some(link);
			return;
		}
		let max_scroll = self.rows.len().saturating_sub(ROWS);
		self.scroll = if down {
			(self.scroll + ROWS - 1).min(max_scroll)
		} else {
			self.scroll.saturating_sub(ROWS - 1)
		};
		match self.next_visible_link(down) {
			Some(link) => self.selected = Some(link),
			None if !self.selected.is_some_and(|link| self.is_visible(link)) => {
				self.selected = None
			}
			None => {}
		}
	}

	fn selected_link(&self) -> Option<&str> {
		let link = self.selected.filter(|&link| self.is_visible(link))?;
		self.links.get(link).map(String::as_str)
	}
}

/// A question the server asked, waiting for an answer.
pub(super) struct Prompt {
	url: Url,
	pub question: String,
	pub answer: TextInput,
}

struct Request {
	url: Url,
	session: Session,
	response: Vec<u8>,
	redirects: u8,
	opened: bool,
	/// Where to scroll to, when going back to a page.
	back_to: Option<usize>,
}

pub(super) struct Browser {
	pub address: TextInput,
	pub editing: bool,
	pub page: Option<Page>,
	history: Vec<(Url, usize)>,
	request: Option<Request>,
	pub prompt: Option<Prompt>,
	pub error: Option<String>,
	/// The certificate each server showed first.
	known: BTreeMap<(String, u16), Fingerprint>,
	config: Option<Arc<rustls::ClientConfig>>,
}

pub(super) enum Outcome {
	Stay,
	Leave,
}

impl Browser {
	pub fn new() -> Self {
		Browser {
			address: TextInput::new("", 60),
			editing: true,
			page: None,
			history: Vec::new(),
			request: None,
			prompt: None,
			error: None,
			known: BTreeMap::new(),
			config: None,
		}
	}

	pub fn is_loading(&self) -> bool {
		self.request.is_some()
	}

	pub fn handle_key(&mut self, key: Key, commands: &mut Vec<Command>) -> Outcome {
		if let Some(prompt) = &mut self.prompt {
			match key {
				Key::Enter => {
					let url = prompt.url.with_query(&prompt.answer.text);
					self.prompt = None;
					self.visit(url, 0, None, commands);
				}
				Key::Escape => self.prompt = None,
				Key::Backspace => prompt.answer.backspace(),
				Key::Char(character) => prompt.answer.push(character),
				_ => {}
			}
			return Outcome::Stay;
		}
		if self.editing {
			match key {
				Key::Enter => match Url::parse(&self.address.text) {
					Some(url) => {
						self.editing = false;
						self.visit(url, 0, None, commands);
					}
					None => self.error = Some("Not a Gemini address".into()),
				},
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
			Some(Key::Escape) if self.request.is_some() => self.cancel(commands),
			Some(Key::Escape | Key::Left) => match self.history.pop() {
				Some((url, scroll)) => self.visit(url, 0, Some(scroll), commands),
				None => return Outcome::Leave,
			},
			Some(Key::Up) => self.page.as_mut().map_or((), |page| page.move_by(false)),
			Some(Key::Down) => self.page.as_mut().map_or((), |page| page.move_by(true)),
			Some(Key::Enter | Key::Right) => self.open_selected(commands),
			_ => {}
		}
		Outcome::Stay
	}

	fn open_selected(&mut self, commands: &mut Vec<Command>) {
		let Some(page) = &self.page else {
			return;
		};
		let Some(link) = page.selected_link() else {
			return;
		};
		match page.url.join(link) {
			Ok(url) => self.visit(url, 0, None, commands),
			Err(LinkError::OtherScheme(scheme)) => {
				self.error = Some(format!("Can't open {scheme} links"));
			}
			Err(LinkError::Invalid) => self.error = Some("Not a valid link".into()),
		}
	}

	fn visit(
		&mut self,
		url: Url,
		redirects: u8,
		back_to: Option<usize>,
		commands: &mut Vec<Command>,
	) {
		self.error = None;
		let Some(address) = url.address() else {
			self.error = Some(format!("Can't look up {}; use an address", url.host));
			return;
		};
		let config = match &self.config {
			Some(config) => config.clone(),
			None => match tls::config() {
				Ok(config) => self.config.insert(config).clone(),
				Err(error) => {
					self.error = Some(error);
					return;
				}
			},
		};
		let session = match Session::new(config, &url.host) {
			Ok(session) => session,
			Err(error) => {
				self.error = Some(error);
				return;
			}
		};
		let to = SocketAddrV6::new(address, url.port, 0, 0);
		commands.push(Command::OpenStream(Stream::Gemini, to));
		self.request = Some(Request {
			url,
			session,
			response: Vec::new(),
			redirects,
			opened: false,
			back_to,
		});
	}

	fn cancel(&mut self, commands: &mut Vec<Command>) {
		if self.request.take().is_some() {
			commands.push(Command::CloseStream(Stream::Gemini));
		}
	}

	pub fn opened(&mut self, commands: &mut Vec<Command>) {
		if let Some(request) = &mut self.request {
			request.opened = true;
			self.progress(commands);
		}
	}

	pub fn received(&mut self, data: &[u8], commands: &mut Vec<Command>) {
		if let Some(request) = &mut self.request {
			request.session.receive(data);
			self.progress(commands);
		}
	}

	pub fn closed(&mut self, commands: &mut Vec<Command>) {
		match &self.request {
			Some(request) if request.opened => self.finish(commands),
			Some(request) => {
				self.error = Some(format!("Could not reach {}", request.url.host));
				self.request = None;
			}
			None => {}
		}
	}

	/// Runs the connection as far as it can go, and sends what it made.
	fn progress(&mut self, commands: &mut Vec<Command>) {
		let Some(request) = &mut self.request else {
			return;
		};
		let output = match request.session.process() {
			Ok(output) => output,
			Err(error) => return self.fail(format!("TLS: {error}"), commands),
		};
		let mut send = output.send;
		if output.connected {
			if let Err(error) = self.check_certificate() {
				return self.fail(error, commands);
			}
			let Some(request) = &mut self.request else {
				return;
			};
			let Some(line) = request.url.request() else {
				return self.fail("The address is too long".into(), commands);
			};
			request.session.write(&line);
			match request.session.process() {
				Ok(more) => send.extend(more.send),
				Err(error) => return self.fail(format!("TLS: {error}"), commands),
			}
		}
		if !send.is_empty() {
			commands.push(Command::StreamSend(Stream::Gemini, send));
		}
		let Some(request) = &mut self.request else {
			return;
		};
		request.response.extend(output.received);
		if output.closed || request.response.len() >= MAX_RESPONSE {
			request.response.truncate(MAX_RESPONSE);
			commands.push(Command::CloseStream(Stream::Gemini));
			self.finish(commands);
		}
	}

	/// Trusts the certificate a server shows first, and refuses another.
	fn check_certificate(&mut self) -> Result<(), String> {
		let request = self.request.as_ref().ok_or_else(String::new)?;
		let fingerprint = request
			.session
			.fingerprint()
			.ok_or("The server showed no certificate")?;
		let server = (request.url.host.clone(), request.url.port);
		match self.known.get(&server) {
			Some(known) if *known != fingerprint => {
				Err(format!("{}'s certificate changed", request.url.host))
			}
			Some(_) => Ok(()),
			None => {
				self.known.insert(server, fingerprint);
				Ok(())
			}
		}
	}

	fn fail(&mut self, error: String, commands: &mut Vec<Command>) {
		self.cancel(commands);
		self.error = Some(error);
	}

	fn finish(&mut self, commands: &mut Vec<Command>) {
		let Some(request) = self.request.take() else {
			return;
		};
		let Some((header, body)) = Header::parse(&request.response) else {
			self.error = Some("The server sent no answer".into());
			return;
		};
		match header.status / 10 {
			1 => {
				let mut answer = TextInput::new("", 200);
				answer.secret = header.status == 11;
				self.prompt = Some(Prompt {
					url: request.url,
					question: header.meta,
					answer,
				});
			}
			2 => self.show(request.url, &header, body, request.back_to),
			3 if request.redirects >= MAX_REDIRECTS => {
				self.error = Some("Too many redirects".into());
			}
			3 => match request.url.join(&header.meta) {
				Ok(url) => self.visit(url, request.redirects + 1, request.back_to, commands),
				Err(_) => self.error = Some(format!("Can't follow a redirect to {}", header.meta)),
			},
			_ => self.error = Some(gemini::describe_failure(&header)),
		}
	}

	fn show(&mut self, url: Url, header: &Header, body: &[u8], back_to: Option<usize>) {
		let media_type = header.meta.split(';').next().unwrap_or("").trim();
		let text = String::from_utf8_lossy(body);
		let (rows, links) = if media_type.is_empty() || media_type == "text/gemini" {
			lay_out(&gemini::parse_gemtext(&text))
		} else if media_type.starts_with("text/") {
			let lines: Vec<Line> = text.lines().map(|line| Line::Text(line.into())).collect();
			lay_out(&lines)
		} else {
			self.error = Some(format!("Can't show {media_type}"));
			return;
		};
		if let Some(previous) = self.page.take()
			&& back_to.is_none()
		{
			if self.history.len() == MAX_HISTORY {
				self.history.remove(0);
			}
			self.history.push((previous.url, previous.scroll));
		}
		let mut page = Page::new(url, rows, links);
		if let Some(scroll) = back_to {
			page.scroll = scroll.min(page.rows.len().saturating_sub(ROWS));
			page.selected = page.first_visible_link();
		}
		self.page = Some(page);
	}
}

/// Wraps a page's lines to the screen, and collects its links.
fn lay_out(lines: &[Line]) -> (Vec<Row>, Vec<String>) {
	let mut rows = Vec::new();
	let mut links = Vec::new();
	for line in lines {
		match line {
			Line::Text(text) => add_wrapped(&mut rows, text, ("", ""), RowKind::Text),
			Line::Heading(text) => add_wrapped(&mut rows, text, ("", ""), RowKind::Heading),
			Line::Item(text) => add_wrapped(&mut rows, text, ("• ", "  "), RowKind::Text),
			Line::Quote(text) => add_wrapped(&mut rows, text, ("> ", "> "), RowKind::Quote),
			Line::Link { url, label } => {
				let kind = RowKind::Link(links.len());
				add_wrapped(&mut rows, label, ("→ ", "  "), kind);
				links.push(url.clone());
			}
			// Wrapping would break up drawings, so long lines are cut.
			Line::Preformatted(text) => rows.push(Row {
				text: text.chars().take(COLUMNS).collect(),
				kind: RowKind::Preformatted,
			}),
		}
	}
	(rows, links)
}

/// Adds `text` as rows, the first starting with `prefixes.0` and the rest
/// with `prefixes.1`.
fn add_wrapped(rows: &mut Vec<Row>, text: &str, prefixes: (&str, &str), kind: RowKind) {
	let width = COLUMNS - prefixes.0.chars().count();
	for (index, part) in wrap(text, width).into_iter().enumerate() {
		let prefix = if index == 0 { prefixes.0 } else { prefixes.1 };
		rows.push(Row {
			text: format!("{prefix}{part}"),
			kind,
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn moves_between_links_and_scrolls_past_text() {
		let mut lines = alloc::vec![Line::Link {
			url: "/a".into(),
			label: "A".into()
		}];
		lines.extend((0..20).map(|_| Line::Text("words".into())));
		lines.push(Line::Link {
			url: "/b".into(),
			label: "B".into(),
		});
		lines.extend((0..5).map(|_| Line::Text("words".into())));
		let (rows, links) = lay_out(&lines);
		let mut page = Page::new(Url::parse("200::1").unwrap(), rows, links);
		assert_eq!((page.selected, page.scroll), (Some(0), 0));
		page.move_by(true);
		assert_eq!((page.selected, page.scroll), (None, 8));
		page.move_by(true);
		assert_eq!((page.selected, page.scroll), (Some(1), 16));
		page.move_by(true);
		assert_eq!((page.selected, page.scroll), (Some(1), 18));
		page.move_by(false);
		assert_eq!((page.selected, page.scroll), (None, 10));
		page.move_by(false);
		page.move_by(false);
		assert_eq!((page.selected, page.scroll), (Some(0), 0));
	}
}
