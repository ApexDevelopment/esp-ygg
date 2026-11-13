//! The Gopher app: the text interface's browser, its pages wrapped to the
//! screen.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use esp_ygg_app::ui::Commands;
use esp_ygg_app::ui::gopher::{Browser, Content};
use slint::{ComponentHandle, ModelRc};

use crate::MainWindow;
use crate::page::Page;

/// As many pages back as the browser keeps.
const MAX_HISTORY: usize = 16;

pub struct Gopher {
	window: MainWindow,
	browser: Browser,
	page: Rc<Page>,
	/// How far each page in the history was scrolled, to return to.
	scrolls: Vec<f32>,
	/// Whether the browser changed since it was last shown.
	changed: bool,
}

impl Gopher {
	pub fn new(window: &MainWindow) -> Self {
		let page = Rc::new(Page::default());
		window
			.global::<crate::Gopher>()
			.set_lines(ModelRc::from(page.clone()));
		Gopher {
			window: window.clone_strong(),
			browser: Browser::new(),
			page,
			scrolls: Vec::new(),
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

	/// Opens a link. Returns whether it is a search waiting for its query.
	pub fn open(&mut self, link: usize, commands: &mut Commands) -> bool {
		self.browser.open(link, commands);
		self.changed = true;
		self.browser.search.is_some()
	}

	pub fn search(&mut self, query: &str, commands: &mut Commands) {
		self.browser.send_search(query, commands);
		self.changed = true;
	}

	pub fn cancel_search(&mut self) {
		self.browser.search = None;
	}

	pub fn back(&mut self) {
		if self.browser.back() {
			let scroll = self.scrolls.pop().unwrap_or_default();
			self.show_page();
			self.global().set_scroll(scroll);
			self.changed = true;
		}
	}

	pub fn fetched(&mut self, data: &[u8]) {
		let had_page = self.browser.page.is_some();
		self.browser.fetched(data);
		if had_page {
			if self.scrolls.len() == MAX_HISTORY {
				self.scrolls.remove(0);
			}
			self.scrolls.push(self.global().get_scroll());
		}
		self.show_page();
		self.global().set_scroll(0.0);
		self.changed = true;
	}

	pub fn failed(&mut self) {
		self.browser.failed();
		self.changed = true;
	}

	/// Shows what changed, and wraps the page to the width the screen gives.
	pub fn refresh(&mut self) {
		self.page.set_width(self.global().get_width());
		if !self.changed {
			return;
		}
		self.changed = false;
		let gopher = self.global();
		gopher.set_address(self.address().into());
		gopher.set_loading(self.browser.is_loading());
		gopher.set_error(self.browser.error.as_deref().unwrap_or_default().into());
		gopher.set_can_go_back(self.browser.can_go_back());
	}

	fn show_page(&self) {
		let Some(page) = &self.browser.page else {
			self.page.set([]);
			return;
		};
		match &page.content {
			Content::Text { text, .. } => {
				self.page.set(text.lines().map(|line| (line, -1, false)));
			}
			Content::Menu { items, .. } => {
				let texts: Vec<String> = items
					.iter()
					.map(|item| match item.kind {
						'1' => format!("{}/", item.text),
						'7' => format!("{}?", item.text),
						_ => item.text.clone(),
					})
					.collect();
				self.page.set(
					items
						.iter()
						.zip(&texts)
						.enumerate()
						.map(|(index, (item, text))| {
							let link = if item.is_link() { index as i32 } else { -1 };
							(text.as_str(), link, item.is_supported())
						}),
				);
			}
		}
	}

	fn global(&self) -> crate::Gopher<'_> {
		self.window.global::<crate::Gopher>()
	}
}
