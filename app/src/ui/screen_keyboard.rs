//! A keyboard drawn on the screen, for boards with a touch screen and no
//! keys. Text fields get letters, with a layer of digits and symbols; fields
//! that only take numbers get a shorter number pad, leaving more of the
//! screen to the rest.

use alloc::string::String;
use core::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};

use yggdrasil_espnow::Instant;

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
pub enum Tapped {
	Key(Key),
	Hide,
}

/// How long a key stays lit after the finger lifts, so even a quick tap
/// shows.
const LIT_AFTER_RELEASE: Duration = Duration::from_millis(120);

const BACKDROP: Color = Color::Rgb(28, 28, 32);
const KEY: Color = Color::Rgb(80, 80, 90);
/// Keys that do something other than type, a shade darker.
const FUNCTION_KEY: Color = Color::Rgb(52, 52, 60);
const LIT: Color = Color::Rgb(210, 210, 220);

#[derive(Default)]
pub struct ScreenKeyboard {
	shown: bool,
	symbols: bool,
	shifted: bool,
	/// The row and place in it of the key under the finger, or just let go.
	pressed: Option<(usize, usize)>,
	released: bool,
	/// When a released key goes dark again, once known.
	lit_until: Option<Instant>,
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

	/// The key at `point` inside `area`, where the keyboard is drawn, as its
	/// row, its place in the row, and the key.
	fn key_at(&self, area: Rect, kind: Kind, point: Position) -> Option<(usize, usize, Cap)> {
		let row = usize::from(point.y.checked_sub(area.y)? / KEY_HEIGHT);
		let keys = *self.rows(kind).get(row)?;
		let (slot_width, margin) = slot_width(area);
		let slot = point.x.checked_sub(area.x + margin)? / slot_width;
		let mut start = 0;
		keys.iter().enumerate().find_map(|(index, &(cap, slots))| {
			let found = (start..start + slots).contains(&slot);
			start += slots;
			found.then_some((row, index, cap))
		})
	}

	/// Lights the key a finger has just touched.
	pub fn press(&mut self, area: Rect, kind: Kind, point: Position) {
		self.pressed = self
			.key_at(area, kind, point)
			.map(|(row, index, _)| (row, index));
		self.released = false;
		self.lit_until = None;
	}

	/// The finger has lifted. The key stays lit a moment longer.
	pub fn release(&mut self) {
		self.released = true;
	}

	pub fn tick(&mut self, now: Instant) {
		if !self.released || self.pressed.is_none() {
			return;
		}
		match self.lit_until {
			None => self.lit_until = Some(now + LIT_AFTER_RELEASE),
			Some(until) if now >= until => {
				self.pressed = None;
				self.lit_until = None;
			}
			Some(_) => {}
		}
	}

	/// Handles a tap at `point` inside `area`, where the keyboard is drawn.
	pub fn tap(&mut self, area: Rect, kind: Kind, point: Position) -> Option<Tapped> {
		let (_, _, cap) = self.key_at(area, kind, point)?;
		Some(match cap {
			C(plain, shifted) => {
				let character = if self.shifted { shifted } else { plain };
				// Shift lasts one letter, but keeps the second page of symbols.
				if !self.symbols {
					self.shifted = false;
				}
				Tapped::Key(Key::Char(character))
			}
			Space => Tapped::Key(Key::Char(' ')),
			Backspace => Tapped::Key(Key::Backspace),
			Enter => Tapped::Key(Key::Enter),
			Tab => Tapped::Key(Key::Tab),
			Hide => Tapped::Hide,
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

	/// Draws the keys on a backdrop. Each key covers its slots but for half
	/// a line above and half a column to the left, drawn with block
	/// characters, which leaves narrow gaps of backdrop between keys.
	pub fn draw(&self, frame: &mut Frame, area: Rect, kind: Kind) {
		let buffer = frame.buffer_mut();
		buffer.set_style(area, Style::new().bg(BACKDROP));
		for y in area.top()..area.bottom() {
			for x in area.left()..area.right() {
				buffer[(x, y)].set_symbol(" ");
			}
		}
		let (slot_width, margin) = slot_width(area);
		for (row, keys) in self.rows(kind).iter().enumerate() {
			let top = area.y + row as u16 * KEY_HEIGHT;
			let mut left = area.x + margin;
			for (index, &(cap, slots)) in keys.iter().enumerate() {
				let width = slots * slot_width;
				let right = (left + width).min(area.right());
				let (face, label_color) = self.colors(cap, self.pressed == Some((row, index)));
				let edge = Style::new().fg(face).bg(BACKDROP);
				buffer[(left, top)].set_symbol("▗").set_style(edge);
				buffer[(left, top + 1)].set_symbol("▐").set_style(edge);
				for x in left + 1..right {
					buffer[(x, top)].set_symbol("▄").set_style(edge);
					buffer[(x, top + 1)]
						.set_symbol(" ")
						.set_style(Style::new().bg(face));
				}
				let label = self.label(cap);
				let inside = right.saturating_sub(left + 1);
				let length = label.chars().count() as u16;
				let start = left + 1 + inside.saturating_sub(length) / 2;
				buffer.set_stringn(
					start,
					top + 1,
					&label,
					usize::from(inside),
					Style::new().fg(label_color).bg(face),
				);
				left += width;
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

	/// The key's face and label colors.
	fn colors(&self, cap: Cap, pressed: bool) -> (Color, Color) {
		match cap {
			_ if pressed => (LIT, Color::Black),
			Enter => (ACCENT, Color::Black),
			Shift if self.shifted => (ACCENT, Color::Black),
			C(..) | Space => (KEY, Color::White),
			_ => (FUNCTION_KEY, Color::White),
		}
	}
}

/// How wide each slot is, and the margin that centers the rows in `area`.
fn slot_width(area: Rect) -> (u16, u16) {
	let width = (area.width / SLOTS).max(1);
	(width, area.width.saturating_sub(width * SLOTS) / 2)
}
