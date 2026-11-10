//! Driving the interface with lines of text, such as over a serial port, so
//! it can be used and tested without someone at the keyboard:
//!
//! - `type <text>` types the text after the space.
//! - `key <name>...` presses keys by name: `enter`, `esc`, `tab`,
//!   `backspace`, `space`, `up`, `down`, `left` and `right`.
//! - `screen` asks for the screen as text.
//! - `talk on` and `talk off` hold and release the push-to-talk button.

use alloc::string::String;
use alloc::vec::Vec;

use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::Key;

/// The lines around a screen sent in answer to `screen`.
pub const SCREEN_START: &str = "--- screen ---";
pub const SCREEN_END: &str = "--- end ---";

#[derive(Debug, PartialEq, Eq)]
pub enum Remote {
	Keys(Vec<Key>),
	Screen,
	Talk(bool),
}

/// Reads one line, or returns why it could not.
pub fn parse(line: &str) -> Result<Remote, String> {
	let line = line.trim_end_matches(['\r', '\n']);
	let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
	match command {
		"type" => Ok(Remote::Keys(rest.chars().map(Key::Char).collect())),
		"key" => rest
			.split_whitespace()
			.map(|name| key_named(name).ok_or_else(|| alloc::format!("No key named {name}")))
			.collect::<Result<_, _>>()
			.map(Remote::Keys),
		"screen" => Ok(Remote::Screen),
		"talk" if rest == "on" => Ok(Remote::Talk(true)),
		"talk" if rest == "off" => Ok(Remote::Talk(false)),
		_ => Err(alloc::format!("Unknown command: {line}")),
	}
}

fn key_named(name: &str) -> Option<Key> {
	Some(match name {
		"enter" => Key::Enter,
		"esc" | "escape" => Key::Escape,
		"tab" => Key::Tab,
		"backspace" => Key::Backspace,
		"space" => Key::Char(' '),
		"up" => Key::Up,
		"down" => Key::Down,
		"left" => Key::Left,
		"right" => Key::Right,
		_ => return None,
	})
}

/// The screen as lines of text.
pub fn screen_text(buffer: &Buffer) -> Vec<String> {
	let area = buffer.area;
	(area.top()..area.bottom())
		.map(|y| {
			let line: String = (area.left()..area.right())
				.map(|x| buffer[(x, y)].symbol())
				.collect();
			String::from(line.trim_end())
		})
		.collect()
}

/// The screen as lines of text, with `>` before lines that have something
/// highlighted, such as the selection.
pub fn marked_screen_text(buffer: &Buffer) -> Vec<String> {
	let area = buffer.area;
	screen_text(buffer)
		.into_iter()
		.zip(area.top()..area.bottom())
		.map(|(line, y)| {
			let highlighted =
				(area.left()..area.right()).any(|x| buffer[(x, y)].bg != Color::Reset);
			let gutter = if highlighted { "> " } else { "  " };
			alloc::format!("{gutter}{line}")
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_commands() {
		assert_eq!(
			parse("type a b\r\n"),
			Ok(Remote::Keys(alloc::vec![
				Key::Char('a'),
				Key::Char(' '),
				Key::Char('b')
			]))
		);
		assert_eq!(
			parse("key down enter"),
			Ok(Remote::Keys(alloc::vec![Key::Down, Key::Enter]))
		);
		assert_eq!(parse("screen"), Ok(Remote::Screen));
		assert_eq!(parse("talk on"), Ok(Remote::Talk(true)));
		assert!(parse("key sideways").is_err());
		assert!(parse("dance").is_err());
	}
}
