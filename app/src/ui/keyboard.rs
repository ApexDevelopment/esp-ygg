//! Turns the keys held on an M5Stack Cardputer into [`Key`] presses. Both
//! models share this layout; only how the held keys are read differs.
//!
//! The layout comes from M5Stack's M5Cardputer library (MIT license,
//! Copyright 2025 M5Stack Technology Co., Ltd.).

use alloc::vec::Vec;
use core::time::Duration;

use yggdrasil_espnow::Instant;

use super::Key;

pub const ROWS: usize = 4;
pub const COLUMNS: usize = 14;

/// How long a key is held before it repeats, and how often it repeats then.
const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_INTERVAL: Duration = Duration::from_millis(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
	Char(char, char),
	Backspace,
	Tab,
	Enter,
	Fn,
	Shift,
	/// Ctrl, Opt and Alt, which nothing uses yet.
	Ignored,
}

use Cap::{Backspace, Char as C, Enter, Fn, Ignored, Shift, Tab};

const LAYOUT: [[Cap; COLUMNS]; ROWS] = [
	[
		C('`', '~'),
		C('1', '!'),
		C('2', '@'),
		C('3', '#'),
		C('4', '$'),
		C('5', '%'),
		C('6', '^'),
		C('7', '&'),
		C('8', '*'),
		C('9', '('),
		C('0', ')'),
		C('-', '_'),
		C('=', '+'),
		Backspace,
	],
	[
		Tab,
		C('q', 'Q'),
		C('w', 'W'),
		C('e', 'E'),
		C('r', 'R'),
		C('t', 'T'),
		C('y', 'Y'),
		C('u', 'U'),
		C('i', 'I'),
		C('o', 'O'),
		C('p', 'P'),
		C('[', '{'),
		C(']', '}'),
		C('\\', '|'),
	],
	[
		Fn,
		Shift,
		C('a', 'A'),
		C('s', 'S'),
		C('d', 'D'),
		C('f', 'F'),
		C('g', 'G'),
		C('h', 'H'),
		C('j', 'J'),
		C('k', 'K'),
		C('l', 'L'),
		C(';', ':'),
		C('\'', '"'),
		Enter,
	],
	[
		Ignored,
		Ignored,
		Ignored,
		C('z', 'Z'),
		C('x', 'X'),
		C('c', 'C'),
		C('v', 'V'),
		C('b', 'B'),
		C('n', 'N'),
		C('m', 'M'),
		C(',', '<'),
		C('.', '>'),
		C('/', '?'),
		C(' ', ' '),
	],
];

/// A key position: row from the top, column from the left.
pub type Position = (u8, u8);

#[derive(Default)]
pub struct Keyboard {
	held: Vec<Position>,
	/// The key that repeats while held, and when it next repeats.
	repeat: Option<(Position, Instant)>,
}

impl Keyboard {
	pub fn new() -> Self {
		Self::default()
	}

	/// Takes every key held right now, and returns the key presses since
	/// the last call, including repeats.
	pub fn update(&mut self, held: &[Position], now: Instant) -> Vec<Key> {
		let fn_held = held.iter().any(|&position| cap(position) == Some(Fn));
		let shift_held = held.iter().any(|&position| cap(position) == Some(Shift));
		let mut keys = Vec::new();
		for &position in held {
			if self.held.contains(&position) {
				continue;
			}
			if let Some(key) = key_for(position, fn_held, shift_held) {
				keys.push(key);
				self.repeat = Some((position, now + REPEAT_DELAY));
			}
		}
		if let Some((position, due)) = self.repeat {
			if !held.contains(&position) {
				self.repeat = None;
			} else if now >= due && keys.is_empty() {
				keys.extend(key_for(position, fn_held, shift_held));
				self.repeat = Some((position, now + REPEAT_INTERVAL));
			}
		}
		self.held = held.to_vec();
		keys
	}
}

fn cap((row, column): Position) -> Option<Cap> {
	LAYOUT
		.get(usize::from(row))?
		.get(usize::from(column))
		.copied()
}

fn key_for(position: Position, fn_held: bool, shift_held: bool) -> Option<Key> {
	Some(match cap(position)? {
		C('`', _) if fn_held => Key::Escape,
		C(';', _) if fn_held => Key::Up,
		C('.', _) if fn_held => Key::Down,
		C(',', _) if fn_held => Key::Left,
		C('/', _) if fn_held => Key::Right,
		C(_, shifted) if shift_held => Key::Char(shifted),
		C(base, _) => Key::Char(base),
		Backspace => Key::Backspace,
		Tab => Key::Tab,
		Enter => Key::Enter,
		Fn | Shift | Ignored => return None,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	const A: Position = (2, 2);
	const FN: Position = (2, 0);
	const SHIFT: Position = (2, 1);
	const SEMICOLON: Position = (2, 11);

	fn at(millis: u64) -> Instant {
		Instant::from_duration(Duration::from_millis(millis))
	}

	#[test]
	fn reports_each_press_once() {
		let mut keyboard = Keyboard::new();
		assert_eq!(keyboard.update(&[A], at(0)), [Key::Char('a')]);
		assert_eq!(keyboard.update(&[A], at(10)), []);
		assert_eq!(keyboard.update(&[], at(20)), []);
		assert_eq!(keyboard.update(&[A], at(30)), [Key::Char('a')]);
	}

	#[test]
	fn applies_shift_and_fn() {
		let mut keyboard = Keyboard::new();
		assert_eq!(keyboard.update(&[SHIFT], at(0)), []);
		assert_eq!(keyboard.update(&[SHIFT, A], at(10)), [Key::Char('A')]);
		assert_eq!(keyboard.update(&[FN], at(20)), []);
		assert_eq!(keyboard.update(&[FN, SEMICOLON], at(30)), [Key::Up]);
		assert_eq!(keyboard.update(&[SEMICOLON], at(40)), []);
		assert_eq!(keyboard.update(&[], at(50)), []);
		assert_eq!(keyboard.update(&[SEMICOLON], at(60)), [Key::Char(';')]);
	}

	#[test]
	fn repeats_a_held_key() {
		let mut keyboard = Keyboard::new();
		assert_eq!(keyboard.update(&[A], at(0)), [Key::Char('a')]);
		assert_eq!(keyboard.update(&[A], at(499)), []);
		assert_eq!(keyboard.update(&[A], at(500)), [Key::Char('a')]);
		assert_eq!(keyboard.update(&[A], at(540)), []);
		assert_eq!(keyboard.update(&[A], at(560)), [Key::Char('a')]);
		assert_eq!(keyboard.update(&[], at(570)), []);
		assert_eq!(keyboard.update(&[], at(1000)), []);
	}
}
