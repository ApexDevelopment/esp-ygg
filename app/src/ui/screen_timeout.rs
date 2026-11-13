//! Dims the screen when it has gone untouched for a while, then turns it
//! off. The touch that wakes a dark screen only wakes it, so it cannot press
//! whatever was under the finger unseen. A locked board shows so for a
//! moment, then goes dark, and nothing wakes it but unlocking.

use core::time::Duration;

const DIM_AFTER: Duration = Duration::from_secs(30);
const OFF_AFTER: Duration = Duration::from_secs(60);
const FADE: Duration = Duration::from_millis(400);
/// How bright the screen looks, out of 255, when dimmed.
const DIM: u8 = 80;
/// How long a locked board shows that it is locked before going dark.
pub const LOCKED_SHOWN: Duration = Duration::from_secs(5);

pub struct ScreenTimeout {
	last_input: Duration,
	/// Whether the finger down now woke the screen.
	waking: bool,
	/// When the board was locked, if it is, and whether the screen dims
	/// while it shows so.
	locked: Option<(Duration, bool)>,
}

impl ScreenTimeout {
	pub fn new(now: Duration) -> Self {
		ScreenTimeout {
			last_input: now,
			waking: false,
			locked: None,
		}
	}

	/// Follows the board's lock, given when it was locked if it is. Unlocking
	/// lights the screen again.
	pub fn set_locked(&mut self, now: Duration, locked_at: Option<Duration>, dim: bool) {
		if self.locked.is_some() && locked_at.is_none() {
			self.last_input = now;
		}
		self.locked = locked_at.map(|at| (at, dim));
	}

	/// Notes input other than touch, such as a key. Returns whether to act
	/// on it, which is not when it woke the screen.
	pub fn input(&mut self, now: Duration) -> bool {
		if self.locked.is_some() {
			return false;
		}
		let off = self.is_off(now);
		self.last_input = now;
		!off
	}

	/// Takes where the screen is touched, if anywhere, and returns the touch
	/// to act on. A touch that wakes the screen is left out until it lifts.
	pub fn touch<P>(&mut self, now: Duration, point: Option<P>) -> Option<P> {
		// A finger still down as the board unlocks waits to lift too.
		if self.locked.is_some() {
			self.waking = point.is_some();
			return None;
		}
		if point.is_none() {
			self.waking = false;
			return None;
		}
		if !self.waking && !self.input(now) {
			self.waking = true;
		}
		self.last_input = now;
		if self.waking { None } else { point }
	}

	pub fn is_off(&self, now: Duration) -> bool {
		self.backlight(now) == 0
	}

	/// The backlight's level, from off at 0 to full at 255.
	pub fn backlight(&self, now: Duration) -> u8 {
		let idle = now.saturating_sub(self.last_input);
		let looks = if let Some((locked_at, dim)) = self.locked {
			let shown = if dim { DIM } else { u8::MAX };
			let locked_for = now.saturating_sub(locked_at);
			if locked_for < LOCKED_SHOWN {
				fade(u8::MAX, shown, locked_for)
			} else {
				fade(shown, 0, locked_for - LOCKED_SHOWN)
			}
		} else if idle < DIM_AFTER {
			u8::MAX
		} else if idle < OFF_AFTER {
			fade(u8::MAX, DIM, idle - DIM_AFTER)
		} else {
			fade(DIM, 0, idle - OFF_AFTER)
		};
		// Light looks about as bright as the square root of its strength.
		(u16::from(looks) * u16::from(looks) / u16::from(u8::MAX)) as u8
	}
}

/// How bright the screen looks `elapsed` into fading from `from` to `to`.
fn fade(from: u8, to: u8, elapsed: Duration) -> u8 {
	if elapsed >= FADE {
		return to;
	}
	let progress = elapsed.as_millis() as i32 * 256 / FADE.as_millis() as i32;
	let level = i32::from(from) + (i32::from(to) - i32::from(from)) * progress / 256;
	level as u8
}

#[cfg(test)]
mod tests {
	use super::*;

	const SECOND: Duration = Duration::from_secs(1);

	#[test]
	fn dims_then_turns_off() {
		let timeout = ScreenTimeout::new(Duration::ZERO);
		let level = |seconds| timeout.backlight(SECOND * seconds);
		assert_eq!(level(29), u8::MAX);
		assert!(level(45) > 0 && level(45) < u8::MAX / 4);
		assert_eq!(level(61), 0);
	}

	#[test]
	fn the_touch_that_wakes_the_screen_is_left_out() {
		let mut timeout = ScreenTimeout::new(Duration::ZERO);
		let mut touch = |seconds, point| timeout.touch(SECOND * seconds, point);
		assert_eq!(touch(90, Some((1, 2))), None);
		assert_eq!(touch(91, Some((5, 2))), None);
		assert_eq!(touch(92, None), None);
		assert_eq!(touch(93, Some((1, 2))), Some((1, 2)));
	}

	#[test]
	fn a_locked_screen_shows_so_then_goes_dark() {
		let mut timeout = ScreenTimeout::new(Duration::ZERO);
		timeout.set_locked(SECOND, Some(SECOND), false);
		assert_eq!(timeout.backlight(SECOND * 5), u8::MAX);
		assert_eq!(timeout.backlight(SECOND * 7), 0);
		timeout.set_locked(SECOND * 8, Some(SECOND * 8), true);
		assert!(timeout.backlight(SECOND * 9) < u8::MAX / 4);
	}

	#[test]
	fn a_locked_screen_takes_nothing_until_unlocked() {
		let mut timeout = ScreenTimeout::new(Duration::ZERO);
		timeout.set_locked(SECOND, Some(SECOND), false);
		assert_eq!(timeout.touch(SECOND * 2, Some((1, 2))), None);
		assert!(!timeout.input(SECOND * 2));
		// The finger still down as it unlocks does nothing either.
		timeout.set_locked(SECOND * 20, None, false);
		assert_eq!(timeout.backlight(SECOND * 20), u8::MAX);
		assert_eq!(timeout.touch(SECOND * 20, Some((1, 2))), None);
		assert_eq!(timeout.touch::<(u8, u8)>(SECOND * 21, None), None);
		assert_eq!(timeout.touch(SECOND * 22, Some((1, 2))), Some((1, 2)));
	}

	#[test]
	fn a_dimmed_screen_takes_touches() {
		let mut timeout = ScreenTimeout::new(Duration::ZERO);
		assert_eq!(timeout.touch(SECOND * 45, Some((1, 2))), Some((1, 2)));
		assert_eq!(timeout.backlight(SECOND * 45), u8::MAX);
	}
}
