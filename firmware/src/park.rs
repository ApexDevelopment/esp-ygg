//! Keeps the UI core off the flash while the main core writes to it. Both
//! cores run their code from flash, which reads back garbage while a sector
//! is erased. Stalling the UI core from outside can catch it halfway through
//! fetching code, so instead it parks itself, spinning in RAM until released.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_time::{Duration, Instant};
use esp_hal::handler;
use esp_hal::interrupt::Priority;
use esp_hal::interrupt::software::SoftwareInterrupt;
use esp_hal::peripherals::FROM_CPU_INTR3;
use log::warn;

/// How long the UI core may take to park before a write is skipped.
const PARK_TIMEOUT: Duration = Duration::from_millis(100);

static ARMED: AtomicBool = AtomicBool::new(false);
/// The latest request to park, the one the UI core is parked for, and the
/// latest released. Numbering them keeps a late answer to an abandoned
/// request from passing for the current one.
static REQUESTED: AtomicU32 = AtomicU32::new(0);
static PARKED: AtomicU32 = AtomicU32::new(0);
static RELEASED: AtomicU32 = AtomicU32::new(0);

/// Lets [`while_parked`] park the calling core. Runs on the UI core, as the
/// interrupt goes to the core that binds it.
#[cfg_attr(not(feature = "cardputer"), allow(dead_code))]
pub fn arm(interrupt: FROM_CPU_INTR3<'static>) {
	SoftwareInterrupt::new(interrupt).set_interrupt_handler(park);
	ARMED.store(true, Ordering::Release);
}

/// Runs `write` with the UI core parked, or returns `None` without running
/// it if the core does not park in time.
pub fn while_parked<R>(write: impl FnOnce() -> R) -> Option<R> {
	if !ARMED.load(Ordering::Acquire) {
		return Some(write());
	}
	let request = REQUESTED.load(Ordering::Relaxed).wrapping_add(1);
	REQUESTED.store(request, Ordering::Release);
	// SAFETY: raising the interrupt only sets its pending bit.
	SoftwareInterrupt::new(unsafe { FROM_CPU_INTR3::steal() }).raise();
	let deadline = Instant::now() + PARK_TIMEOUT;
	while PARKED.load(Ordering::Acquire) != request {
		if Instant::now() >= deadline {
			RELEASED.store(request, Ordering::Release);
			warn!("The UI core did not pause for a flash write, so it was skipped");
			return None;
		}
	}
	let result = write();
	RELEASED.store(request, Ordering::Release);
	Some(result)
}

#[handler(priority = Priority::Priority3)]
fn park() {
	// SAFETY: clearing the interrupt only clears its pending bit.
	SoftwareInterrupt::new(unsafe { FROM_CPU_INTR3::steal() }).reset();
	spin(REQUESTED.load(Ordering::Acquire));
}

/// Waits in RAM until the write for `request` is over.
#[esp_hal::ram]
fn spin(request: u32) {
	if RELEASED.load(Ordering::Acquire) == request {
		return;
	}
	PARKED.store(request, Ordering::Release);
	while RELEASED.load(Ordering::Acquire) != request {}
}
