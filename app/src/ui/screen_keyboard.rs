//! A keyboard drawn on the screen, for boards with a touch screen and no
//! keys. Text fields get letters, with a layer of digits and symbols; fields
//! that only take numbers get a shorter number pad, leaving more of the
//! screen to the rest.

use alloc::string::String;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::Key;
use super::view::ACCENT;

/// What a text field takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Text,
	/// Digits and the punctuation of numbers, addresses and offsets.
	Number,
}

/// Every row has this many slots, and a key covers one or more of them.
const SLOTS: u16 = 10;
/// Each row of keys is this many lines tall, to be big enough to tap.
const KEY_HEIGHT: u16 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
	/// A character, and the one Shift gives.
	Char(char, char),
	Backspace,
	Enter,
	Tab,
	Space,
	Shift,
	Symbols,
	Letters,
	Hide,
}

use Cap::{Backspace, Char as C, Enter, Hide, Letters, Shift, Space, Symbols, Tab};

type Row = &'static [(Cap, u16)];

const fn letter(lower: char) -> (Cap, u16) {
	(C(lower, lower.to_ascii_uppercase()), 1)
}

const LETTERS: [Row; 4] = [
	&[
		letter('q'),
		letter('w'),
		letter('e'),
		letter('r'),
		letter('t'),
		letter('y'),
		letter('u'),
		letter('i'),
		letter('o'),
		letter('p'),
	],
	&[
		letter('a'),
		letter('s'),
		letter('d'),
		letter('f'),
		letter('g'),
		letter('h'),
		letter('j'),
		letter('k'),
		letter('l'),
		(Backspace, 1),
	],
	&[
		(Shift, 1),
		letter('z'),
		letter('x'),
		letter('c'),
		letter('v'),
		letter('b'),
		letter('n'),
		letter('m'),
		(C('.', ','), 1),
		(Enter, 1),
	],
	BOTTOM_ROW_LETTERS,
];

/// Shift on this layer turns to a second page of symbols.
const SYMBOLS: [Row; 4] = [
	&[
		(C('1', '1'), 1),
		(C('2', '2'), 1),
		(C('3', '3'), 1),
		(C('4', '4'), 1),
		(C('5', '5'), 1),
		(C('6', '6'), 1),
		(C('7', '7'), 1),
		(C('8', '8'), 1),
		(C('9', '9'), 1),
		(C('0', '0'), 1),
	],
	&[
		(C('@', '$'), 1),
		(C('#', '%'), 1),
		(C('_', '^'), 1),
		(C('&', '~'), 1),
		(C('+', '`'), 1),
		(C('(', '\\'), 1),
		(C(')', '|'), 1),
		(C('[', '{'), 1),
		(C(']', '}'), 1),
		(Backspace, 1),
	],
	&[
		(Shift, 1),
		(C('\'', '<'), 1),
		(C('"', '>'), 1),
		(C('!', '!'), 1),
		(C('?', '?'), 1),
		(C(',', ','), 1),
		(C(';', ';'), 1),
		(C('=', '='), 1),
		(C('*', '*'), 1),
		(Enter, 1),
	],
	BOTTOM_ROW_SYMBOLS,
];

const BOTTOM_ROW_LETTERS: Row = &[
	(Symbols, 1),
	(Tab, 1),
	(Space, 4),
	(C(':', ':'), 1),
	(C('-', '-'), 1),
	(C('/', '/'), 1),
	(Hide, 1),
];

const BOTTOM_ROW_SYMBOLS: Row = &[
	(Letters, 1),
	(Tab, 1),
	(Space, 4),
	(C(':', ':'), 1),
	(C('-', '-'), 1),
	(C('/', '/'), 1),
	(Hide, 1),
];

const NUMBERS: [Row; 2] = [
	SYMBOLS[0],
	&[
		(C('.', '.'), 1),
		(C(':', ':'), 1),
		(C('-', '-'), 1),
		(C('+', '+'), 1),
		(Backspace, 2),
		(Enter, 2),
		(Hide, 2),
	],
];

/// What a tap on the keyboard did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
	Key(Key),
	Hide,
}

#[derive(Default)]
pub struct ScreenKeyboard {
	shown: bool,
	symbols: bool,
	shifted: bool,
}

impl ScreenKeyboard {
	pub fn is_shown(&self) -> bool {
		self.shown
	}

	/// Shows the keyboard, starting from lowercase letters.
	pub fn show(&mut self) {
		*self = ScreenKeyboard {
			shown: true,
			..Default::default()
		};
	}

	pub fn hide(&mut self) {
		self.shown = false;
	}

	/// How many lines the keyboard takes for a field of this kind.
	pub fn height(kind: Kind) -> u16 {
		Self::rows_for(kind, false).len() as u16 * KEY_HEIGHT
	}

	fn rows(&self, kind: Kind) -> &'static [Row] {
		Self::rows_for(kind, self.symbols)
	}

	fn rows_for(kind: Kind, symbols: bool) -> &'static [Row] {
		match (kind, symbols) {
			(Kind::Number, _) => &NUMBERS,
			(Kind::Text, false) => &LETTERS,
			(Kind::Text, true) => &SYMBOLS,
		}
	}

	/// Handles a tap at `point` inside `area`, where the keyboard is drawn.
	pub fn tap(&mut self, area: Rect, kind: Kind, point: Position) -> Option<Press> {
		let row = usize::from((point.y - area.y) / KEY_HEIGHT);
		let keys = *self.rows(kind).get(row)?;
		let (slot_width, margin) = slot_width(area);
		let slot = (point.x - area.x).checked_sub(margin)? / slot_width;
		let cap = cap_at(keys, slot)?;
		Some(match cap {
			C(plain, shifted) => {
				let character = if self.shifted { shifted } else { plain };
				// Shift lasts one letter, but keeps the second page of symbols.
				if !self.symbols {
					self.shifted = false;
				}
				Press::Key(Key::Char(character))
			}
			Space => Press::Key(Key::Char(' ')),
			Backspace => Press::Key(Key::Backspace),
			Enter => Press::Key(Key::Enter),
			Tab => Press::Key(Key::Tab),
			Hide => Press::Hide,
			Shift => {
				self.shifted = !self.shifted;
				return None;
			}
			Symbols | Letters => {
				self.symbols = cap == Symbols;
				self.shifted = false;
				return None;
			}
		})
	}

	pub fn draw(&self, frame: &mut Frame, area: Rect, kind: Kind) {
		let (slot_width, margin) = slot_width(area);
		for (index, keys) in self.rows(kind).iter().enumerate() {
			let y = area.y + index as u16 * KEY_HEIGHT;
			let mut slot = 0;
			for &(cap, slots) in keys.iter() {
				let face = Rect {
					x: area.x + margin + slot * slot_width,
					y,
					// A column between keys keeps them apart.
					width: (slots * slot_width).saturating_sub(1),
					height: KEY_HEIGHT,
				}
				.intersection(area);
				let label = Line::from(self.label(cap)).centered();
				frame.render_widget(Paragraph::new(label).style(self.style(cap)), face);
				slot += slots;
			}
		}
	}

	fn label(&self, cap: Cap) -> String {
		match cap {
			C(plain, shifted) => String::from(if self.shifted { shifted } else { plain }),
			Backspace => "⌫".into(),
			Enter => "⏎".into(),
			Tab => "⇥".into(),
			Space => "space".into(),
			Shift => "⇧".into(),
			Symbols => "123".into(),
			Letters => "abc".into(),
			Hide => "▼".into(),
		}
	}

	fn style(&self, cap: Cap) -> Style {
		match cap {
			C(..) | Space => Style::new().fg(Color::White).bg(Color::DarkGray),
			Enter => Style::new().fg(Color::Black).bg(ACCENT),
			Shift if self.shifted => Style::new().fg(Color::Black).bg(ACCENT),
			_ => Style::new().fg(Color::Black).bg(Color::Gray),
		}
	}
}

/// How wide each slot is, and the margin that centers the rows in `area`.
fn slot_width(area: Rect) -> (u16, u16) {
	let width = (area.width / SLOTS).max(1);
	(width, area.width.saturating_sub(width * SLOTS) / 2)
}

fn cap_at(keys: Row, slot: u16) -> Option<Cap> {
	let mut start = 0;
	keys.iter().find_map(|&(cap, slots)| {
		let found = (start..start + slots).contains(&slot);
		start += slots;
		found.then_some(cap)
	})
}
