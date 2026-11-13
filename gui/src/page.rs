//! A page of text and links, wrapped to the screen's width, for a list
//! that makes only the rows showing. Rows are cut from one string as they
//! are shown, so a long page costs little more than its text.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::ops::Range;

use slint::{Model, ModelNotify, ModelTracker};

use crate::{LineStyle, PageLine};

include!(concat!(env!("OUT_DIR"), "/advances.rs"));

/// Spaces a tab goes to the next multiple of.
const TAB_STOP: usize = 8;
/// How far a quote is set in, leaving room for its bar, in pixels.
const QUOTE_INDENT: u32 = 12;
/// The text size the advances are for, and the size of headings, which
/// must match the page's.
const BODY_SIZE: u32 = 14;
const HEADING_SIZE: u32 = 16;
/// The bullet that begins an item of a list.
pub const BULLET: &str = "• ";

/// A line before wrapping.
struct Line {
	text: Range<usize>,
	/// The link it is part of, or -1.
	link: i32,
	style: LineStyle,
}

struct Row {
	text: Range<usize>,
	line: usize,
	/// In pixels.
	indent: u32,
}

#[derive(Default)]
pub struct Page {
	text: RefCell<String>,
	lines: RefCell<Vec<Line>>,
	rows: RefCell<Vec<Row>>,
	/// The width wrapped to, in 64ths of a pixel.
	width: Cell<u32>,
	notify: ModelNotify,
}

impl Page {
	/// Shows these lines in place of the page's: their text, the link each
	/// is part of or -1, and their style.
	pub fn set<'a>(&self, lines: impl IntoIterator<Item = (&'a str, i32, LineStyle)>) {
		let mut text = self.text.borrow_mut();
		let mut kept = self.lines.borrow_mut();
		text.clear();
		kept.clear();
		for (line, link, style) in lines {
			let start = text.len();
			expand_tabs(line, &mut text);
			kept.push(Line {
				text: start..text.len(),
				link,
				style,
			});
		}
		drop((text, kept));
		self.wrap();
	}

	/// Wraps lines to `width`, in pixels, if it changed.
	pub fn set_width(&self, width: f32) {
		let width = (width.max(0.0) * 64.0) as u32;
		if width != self.width.get() {
			self.width.set(width);
			self.wrap();
		}
	}

	fn wrap(&self) {
		let text = self.text.borrow();
		let mut rows = self.rows.borrow_mut();
		rows.clear();
		let width = self.width.get();
		for (index, line) in self.lines.borrow().iter().enumerate() {
			let start = line.text.start;
			let source = &text[line.text.clone()];
			let (first, rest) = match line.style {
				LineStyle::Quote => (QUOTE_INDENT, QUOTE_INDENT),
				// Lines after the first start under the item's text.
				LineStyle::Item => (0, width_of(BULLET, &ADVANCES) / 64),
				_ => (0, 0),
			};
			let ranges = match line.style {
				LineStyle::Preformatted => core::iter::once(0..source.len()).collect(),
				// Headings are larger, so as much fits as in less room at body size.
				LineStyle::Heading => {
					let room = width * BODY_SIZE / HEADING_SIZE;
					wrap(source, room, (first, rest), &MEDIUM_ADVANCES)
				}
				_ => wrap(source, width, (first, rest), &ADVANCES),
			};
			for (number, range) in ranges.into_iter().enumerate() {
				rows.push(Row {
					text: start + range.start..start + range.end,
					line: index,
					indent: if number == 0 { first } else { rest },
				});
			}
		}
		drop(rows);
		self.notify.reset();
	}
}

impl Model for Page {
	type Data = PageLine;

	fn row_count(&self) -> usize {
		self.rows.borrow().len()
	}

	fn row_data(&self, row: usize) -> Option<PageLine> {
		let rows = self.rows.borrow();
		let row = rows.get(row)?;
		let line = &self.lines.borrow()[row.line];
		Some(PageLine {
			text: self.text.borrow()[row.text.clone()].into(),
			link: line.link,
			style: line.style,
			indent: row.indent as f32,
		})
	}

	fn model_tracker(&self) -> &dyn ModelTracker {
		&self.notify
	}
}

fn expand_tabs(line: &str, into: &mut String) {
	let mut column = 0;
	for character in line.chars() {
		if character == '\t' {
			let spaces = TAB_STOP - column % TAB_STOP;
			into.extend(core::iter::repeat_n(' ', spaces));
			column += spaces;
		} else {
			into.push(character);
			column += 1;
		}
	}
}

/// How far a character moves the pen, in 64ths of a pixel, in a font's
/// table of advances. Others than ASCII are taken to be as wide as M, to be
/// sure they fit.
fn advance(character: char, advances: &[u32; 95]) -> u32 {
	let index = match character {
		' '..='~' => character as usize - ' ' as usize,
		_ => 'M' as usize - ' ' as usize,
	};
	advances[index]
}

fn width_of(text: &str, advances: &[u32; 95]) -> u32 {
	text.chars()
		.map(|character| advance(character, advances))
		.sum()
}

/// Splits a line into rows no wider than `width`, in 64ths of a pixel, at
/// spaces where it can, as byte ranges. The first row and the rest are set
/// in by `indents`, in pixels. An empty line is one empty row.
fn wrap(line: &str, width: u32, indents: (u32, u32), advances: &[u32; 95]) -> Vec<Range<usize>> {
	// Not yet laid out
	if width == 0 {
		return core::iter::once(0..line.len()).collect();
	}
	let mut rows = Vec::new();
	let mut start = 0;
	let mut used = 0;
	let mut room = width.saturating_sub(indents.0 * 64);
	// The last space in the row so far, to break at.
	let mut space = None;
	for (index, character) in line.char_indices() {
		let advance = advance(character, advances);
		if used + advance > room && index > start {
			room = width.saturating_sub(indents.1 * 64);
			if character == ' ' {
				rows.push(start..index);
				start = index + 1;
				used = 0;
				space = None;
				continue;
			}
			let end = space.filter(|&space| space > start).unwrap_or(index);
			rows.push(start..end);
			start = if Some(end) == space { end + 1 } else { end };
			used = width_of(&line[start..index], advances);
			space = None;
		}
		if character == ' ' {
			space = Some(index);
		}
		used += advance;
	}
	rows.push(start..line.len());
	rows
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rows(line: &str, characters: u32) -> Vec<&str> {
		let width = characters * advance('n', &ADVANCES);
		wrap(line, width, (0, 0), &ADVANCES)
			.into_iter()
			.map(|range| &line[range])
			.collect()
	}

	#[test]
	fn breaks_at_spaces() {
		assert_eq!(rows("nnn nnn nnn", 7), ["nnn nnn", "nnn"]);
	}

	#[test]
	fn breaks_words_too_long_for_a_row() {
		assert_eq!(rows("nnnnnnnnnn", 4), ["nnnn", "nnnn", "nn"]);
	}

	#[test]
	fn keeps_empty_lines() {
		assert_eq!(rows("", 4), [""]);
	}

	#[test]
	fn drops_the_space_at_a_break() {
		assert_eq!(rows("nnnn nnnn", 4), ["nnnn", "nnnn"]);
	}

	#[test]
	fn narrows_rows_after_the_first_by_their_indent() {
		let n = advance('n', &ADVANCES);
		let line = "nnnn nnnn nnnn";
		// Set in by a little over five letters, rows after the first fit three.
		let wrapped = wrap(line, 9 * n, (0, 5 * n / 64 + 1), &ADVANCES);
		let rows: Vec<&str> = wrapped.into_iter().map(|range| &line[range]).collect();
		assert_eq!(rows, ["nnnn nnnn", "nnn", "n"]);
	}

	#[test]
	fn expands_tabs_to_the_next_stop() {
		let mut text = String::new();
		expand_tabs("ab\tc", &mut text);
		assert_eq!(text, "ab      c");
	}
}
