//! The button that locks the board against input, so it can go in a
//! pocket, and unlocks it again: G0 on a Cardputer, BOOT on the touch
//! board. Each interface shows the lock its own way, then goes dark.

use core::cell::Cell;
use core::time::Duration;

use embassy_sync::blocking_mutex::CriticalSectionMutex;

use crate::ui::uptime;

/// Presses closer together than this are the button bouncing.
const BOUNCE: Duration = Duration::from_millis(250);

/// When the board was locked, if it is, and when it last changed.
static LOCK: CriticalSectionMutex<Cell<(Option<Duration>, Duration)>> =
	CriticalSectionMutex::new(Cell::new((None, Duration::ZERO)));

/// Locks the board, or unlocks it, as a press of the button does.
pub fn toggle() {
	let now = uptime();
	LOCK.lock(|lock| {
		let (locked_at, changed) = lock.get();
		if changed != Duration::ZERO && now.saturating_sub(changed) < BOUNCE {
			return;
		}
		let locked_at = match locked_at {
			Some(_) => None,
			None => Some(now),
		};
		lock.set((locked_at, now));
	});
}

#[cfg(feature = "voice")]
pub fn unlock() {
	LOCK.lock(|lock| lock.set((None, uptime())));
}

/// When the board was locked, if it is.
pub fn locked_at() -> Option<Duration> {
	LOCK.lock(|lock| lock.get().0)
}

#[cfg(feature = "tui")]
pub fn is_locked() -> bool {
	locked_at().is_some()
}

/// Whether the board has been locked long enough for its screen to go dark.
#[cfg(feature = "cardputer")]
pub fn is_dark() -> bool {
	use esp_ygg_app::ui::screen_timeout::LOCKED_SHOWN;
	locked_at().is_some_and(|at| uptime().saturating_sub(at) >= LOCKED_SHOWN)
}

/// Notices presses of the lock button. In voice builds, the audio task
/// does, as the button also talks.
#[cfg(not(feature = "voice"))]
#[derive(Default)]
pub struct Button {
	held: bool,
}

#[cfg(not(feature = "voice"))]
impl Button {
	/// Takes whether the button is held now, and locks or unlocks the board
	/// as it goes down.
	pub fn update(&mut self, held: bool) {
		if held && !self.held {
			toggle();
		}
		self.held = held;
	}
}
