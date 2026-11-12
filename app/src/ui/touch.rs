//! Turns a touch screen's samples into [`Key`] presses, for boards without
//! a keyboard. Dragging up or down moves the selection a row at a time,
//! following the finger. A tap is Enter, and a swipe to the right is Escape.

use alloc::vec::Vec;

use super::Key;

/// A point on the screen in pixels, from the top left.
pub type Point = (u16, u16);

/// How far a finger drags for each row the selection moves.
const STEP: u16 = 24;
/// A touch that strays further than this is not a tap.
const TAP_SLOP: u16 = 10;
/// How far right a swipe goes to count as Escape.
const SWIPE: u16 = 60;

#[derive(Default)]
pub struct Gestures {
	touch: Option<Touch>,
}

struct Touch {
	start: Point,
	last: Point,
	/// Where the next row's worth of dragging is counted from.
	anchor: u16,
	dragged: bool,
}

impl Gestures {
	pub fn new() -> Self {
		Self::default()
	}

	/// Takes where the screen is touched right now, if anywhere, and returns
	/// the key presses since the last call.
	pub fn update(&mut self, point: Option<Point>) -> Vec<Key> {
		let mut keys = Vec::new();
		match (point, &mut self.touch) {
			(Some(point), None) => {
				self.touch = Some(Touch {
					start: point,
					last: point,
					anchor: point.1,
					dragged: false,
				});
			}
			(Some(point), Some(touch)) => {
				touch.last = point;
				if touch.mostly_vertical() {
					touch.drag(&mut keys);
				}
			}
			(None, Some(touch)) => {
				keys.extend(touch.release());
				self.touch = None;
			}
			(None, None) => {}
		}
		keys
	}
}

impl Touch {
	fn mostly_vertical(&self) -> bool {
		self.last.1.abs_diff(self.start.1) > self.last.0.abs_diff(self.start.0)
	}

	fn drag(&mut self, keys: &mut Vec<Key>) {
		let y = self.last.1;
		while y >= self.anchor + STEP {
			keys.push(Key::Down);
			self.anchor += STEP;
			self.dragged = true;
		}
		while y + STEP <= self.anchor {
			keys.push(Key::Up);
			self.anchor -= STEP;
			self.dragged = true;
		}
	}

	fn release(&self) -> Option<Key> {
		if self.dragged {
			return None;
		}
		let across = self.last.0.abs_diff(self.start.0);
		let down = self.last.1.abs_diff(self.start.1);
		if self.last.0 > self.start.0 && across >= SWIPE && across > down {
			Some(Key::Escape)
		} else if across <= TAP_SLOP && down <= TAP_SLOP {
			Some(Key::Enter)
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn run(points: &[Option<Point>]) -> Vec<Key> {
		let mut gestures = Gestures::new();
		points
			.iter()
			.flat_map(|&point| gestures.update(point))
			.collect()
	}

	#[test]
	fn a_steady_touch_is_enter() {
		assert_eq!(
			run(&[Some((100, 100)), Some((104, 97)), None]),
			[Key::Enter]
		);
	}

	#[test]
	fn a_swipe_right_is_escape_and_left_is_nothing() {
		assert_eq!(
			run(&[Some((50, 100)), Some((90, 104)), Some((130, 108)), None]),
			[Key::Escape]
		);
		assert_eq!(run(&[Some((130, 100)), Some((50, 100)), None]), []);
	}

	#[test]
	fn dragging_moves_a_row_per_step_and_ends_without_enter() {
		let keys = run(&[
			Some((100, 100)),
			Some((101, 100 + STEP * 2 + 5)),
			Some((101, 100 - STEP)),
			None,
		]);
		assert_eq!(keys, [Key::Down, Key::Down, Key::Up, Key::Up, Key::Up]);
	}
}
