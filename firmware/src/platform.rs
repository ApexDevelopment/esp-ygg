use core::time::Duration;

use esp_hal::rng::Rng;
use yggdrasil_espnow::{Instant, Platform};

/// The clock and random number generator the node runs on.
#[derive(Clone)]
pub struct EspPlatform {
	rng: Rng,
	/// Unix time at boot, as far as the node can tell. See [`crate::storage`].
	unix_base: u64,
}

impl EspPlatform {
	pub fn new(rng: Rng, unix_base: u64) -> Self {
		EspPlatform { rng, unix_base }
	}
}

impl Platform for EspPlatform {
	fn now(&self) -> Instant {
		Instant::from_duration(Duration::from_micros(
			embassy_time::Instant::now().as_micros(),
		))
	}

	fn unix_time(&self) -> u64 {
		self.unix_base + embassy_time::Instant::now().as_secs()
	}

	fn fill_random(&mut self, buf: &mut [u8]) {
		self.rng.read(buf);
	}
}

pub fn to_embassy(instant: Instant) -> embassy_time::Instant {
	let micros = instant.as_duration().as_micros();
	embassy_time::Instant::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
}

/// The current time, as the node and the UI count it.
#[cfg(feature = "tui")]
pub fn now() -> Instant {
	Instant::from_duration(Duration::from_micros(
		embassy_time::Instant::now().as_micros(),
	))
}

/// Randomness for TLS. The hardware generator is truly random while the
/// radio runs, which it always does here.
#[cfg(feature = "gemini")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
	destination: *mut u8,
	length: usize,
) -> Result<(), getrandom::Error> {
	// SAFETY: getrandom passes a buffer of this length that it owns.
	let buffer = unsafe { core::slice::from_raw_parts_mut(destination, length) };
	Rng::new().read(buffer);
	Ok(())
}
