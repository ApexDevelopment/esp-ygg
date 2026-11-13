//! The Gemini app: the text interface's browser, its pages wrapped to the
//! screen.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use esp_ygg_app::gemini::Line;
use esp_ygg_app::ui::Commands;
use esp_ygg_app::ui::gemini::Browser;
use slint::{ComponentHandle, ModelRc};

use crate::page::{BULLET, Page};
use crate::{LineStyle, MainWindow};

/// As many pages back as the browser keeps.
const MAX_HISTORY: usize = 16;

pub struct Gemini {
	window: MainWindow,
	browser: Browser,
	page: Rc<Page>,
	/// How many pages the browser had shown when last looked at.
	shown: usize,
	/// How far each page in the history was scrolled, to return to.
	scrolls: Vec<f32>,
	/// Where to scroll the page being gone back to, once it comes.
	back_to: Option<f32>,
	/// Whether the keyboard is up for the question a server asked.
	answering: bool,
	/// Whether the browser changed since it was last shown.
	changed: bool,
}

impl Gemini {
	pub fn new(window: &MainWindow) -> Self {
		let page = Rc::new(Page::default());
		window
			.global::<crate::Gemini>()
			.set_lines(ModelRc::from(page.clone()));
		Gemini {
			window: window.clone_strong(),
			browser: Browser::without_layout(),
			page,
			shown: 0,
			scrolls: Vec::new(),
			back_to: None,
			answering: false,
			changed: true,
		}
	}

	/// The page showing's address, if any.
	pub fn address(&self) -> String {
		let page = self.browser.page.as_ref();
		page.map(|page| page.url.to_string()).unwrap_or_default()
	}

	pub fn go_to(&mut self, text: &str, commands: &mut Commands) {
		self.browser.go_to(text, commands);
		self.changed = true;
	}

	pub fn open(&mut self, link: usize, commands: &mut Commands) {
		self.browser.open(link, commands);
		self.changed = true;
	}

	pub fn back(&mut self, commands: &mut Commands) {
		if self.browser.back(commands) {
			self.back_to = Some(self.scrolls.pop().unwrap_or_default());
			self.changed = true;
		}
	}

	pub fn stop(&mut self, commands: &mut Commands) {
		self.browser.cancel(commands);
		self.changed = true;
	}

	/// Sends the answer to the question the server asked.
	pub fn answer(&mut self, text: &str, commands: &mut Commands) {
		self.answering = false;
		self.browser.answer(text, commands);
		self.changed = true;
	}

	pub fn cancel_answer(&mut self) {
		self.answering = false;
		self.browser.prompt = None;
	}

	pub fn opened(&mut self, commands: &mut Commands) {
		self.browser.opened(commands);
		self.changed = true;
	}

	pub fn received(&mut self, data: &[u8], commands: &mut Commands) {
		self.browser.received(data, commands);
		self.changed = true;
	}

	pub fn closed(&mut self, commands: &mut Commands) {
		self.browser.closed(commands);
		self.changed = true;
	}

	/// Shows what changed, and wraps the page to the width the screen
	/// gives. Returns a question the server just asked, and whether its
	/// answer is secret, for the keyboard.
	pub fn refresh(&mut self) -> Option<(String, bool)> {
		self.page.set_width(self.global().get_width());
		if !self.changed {
			return None;
		}
		self.changed = false;
		if self.browser.shown != self.shown {
			self.shown = self.browser.shown;
			self.show_page();
		}
		let gemini = self.global();
		gemini.set_address(self.address().into());
		gemini.set_loading(self.browser.is_loading());
		gemini.set_error(self.browser.error.as_deref().unwrap_or_default().into());
		gemini.set_can_go_back(self.browser.can_go_back());
		let prompt = self.browser.prompt.as_ref().filter(|_| !self.answering)?;
		self.answering = true;
		Some((prompt.question.clone(), prompt.answer.secret))
	}

	/// Shows the page the browser just got, taking its lines.
	fn show_page(&mut self) {
		let Some(page) = &mut self.browser.page else {
			return;
		};
		let mut link = 0;
		let rows: Vec<(String, i32, LineStyle)> = core::mem::take(&mut page.lines)
			.into_iter()
			.map(|line| match line {
				Line::Text(text) => (text, -1, LineStyle::Text),
				Line::Heading(text) => (text, -1, LineStyle::Heading),
				Line::Item(text) => (format!("{BULLET}{text}"), -1, LineStyle::Item),
				Line::Quote(text) => (text, -1, LineStyle::Quote),
				Line::Preformatted(text) => (text, -1, LineStyle::Preformatted),
				Line::Link { label, .. } => {
					link += 1;
					(label, link - 1, LineStyle::Link)
				}
			})
			.collect();
		self.page.set(
			rows.iter()
				.map(|(text, link, style)| (text.as_str(), *link, *style)),
		);
		let scroll = match self.back_to.take() {
			Some(scroll) => scroll,
			None => {
				if self.shown > 1 {
					if self.scrolls.len() == MAX_HISTORY {
						self.scrolls.remove(0);
					}
					self.scrolls.push(self.global().get_scroll());
				}
				0.0
			}
		};
		self.global().set_scroll(scroll);
	}

	fn global(&self) -> crate::Gemini<'_> {
		self.window.global::<crate::Gemini>()
	}
}
