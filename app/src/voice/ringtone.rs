//! The tones played while a call rings, at a chosen volume.

use super::SAMPLE_RATE;

pub const RINGTONES: &[&str] = &["Classic", "Warble", "Chime", "Beeps"];
/// The loudest volume. Zero is silent.
pub const LOUDEST: u8 = 5;

/// The peak of each volume, roughly doubling at each step.
const PEAKS: [i32; LOUDEST as usize + 1] = [0, 2000, 4000, 8000, 14000, 24000];
/// The peak of a waveform before the volume scales it.
const UNIT: i32 = 1024;

/// The sample at `index` of ringtone `tone`, which repeats.
pub fn ring(tone: u8, volume: u8, index: usize) -> i16 {
	let shape = match tone {
		1 => warble(index),
		2 => chime(index),
		3 => beeps(index),
		_ => classic(index),
	};
	let peak = PEAKS[usize::from(volume.min(LOUDEST))];
	(shape * peak / UNIT) as i16
}

const fn millis(ms: usize) -> usize {
	SAMPLE_RATE as usize * ms / 1000
}

/// A triangle wave, gentler than a square on a small speaker.
fn triangle(index: usize, hertz: u32) -> i32 {
	let phase = (index as u64 * u64::from(hertz) * 4096 / u64::from(SAMPLE_RATE) % 4096) as i32;
	if phase < 2048 {
		phase - UNIT
	} else {
		3 * UNIT - phase
	}
}

/// Two short bursts, then a pause, like a British telephone.
fn classic(index: usize) -> i32 {
	const ON: usize = millis(400);
	const OFF: usize = millis(200);
	let at = index % (2 * (ON + OFF) + millis(2000));
	let sounding = at < ON || (ON + OFF..2 * ON + OFF).contains(&at);
	if sounding { triangle(at, 400) } else { 0 }
}

/// Two pitches trading places quickly, then a pause.
fn warble(index: usize) -> i32 {
	let at = index % millis(3000);
	if at >= millis(1000) {
		return 0;
	}
	let hertz = if (at / millis(50)).is_multiple_of(2) {
		650
	} else {
		850
	};
	triangle(at, hertz)
}

/// Three falling notes that fade, then a pause.
fn chime(index: usize) -> i32 {
	const NOTE: usize = millis(300);
	const NOTES: [u32; 3] = [784, 659, 523];
	let at = index % (NOTES.len() * NOTE + millis(1500));
	let Some(&hertz) = NOTES.get(at / NOTE) else {
		return 0;
	};
	let left = (NOTE - at % NOTE) as i32;
	triangle(at, hertz) * left / NOTE as i32
}

/// Four quick beeps, then a pause.
fn beeps(index: usize) -> i32 {
	const BEEP: usize = millis(100);
	let at = index % (8 * BEEP + millis(1200));
	if at < 8 * BEEP && (at / BEEP).is_multiple_of(2) {
		triangle(at, 1000)
	} else {
		0
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn loudest(tone: u8, volume: u8, samples: core::ops::Range<usize>) -> i32 {
		samples
			.map(|index| i32::from(ring(tone, volume, index)).abs())
			.max()
			.unwrap_or(0)
	}

	#[test]
	fn every_ringtone_sounds_then_pauses() {
		for tone in 0..RINGTONES.len() as u8 {
			assert!(loudest(tone, LOUDEST, 0..800) > 10000, "tone {tone}");
			let pause = millis(1300)..millis(1800);
			assert_eq!(loudest(tone, LOUDEST, pause), 0, "tone {tone}");
		}
	}

	#[test]
	fn volume_scales_and_zero_is_silent() {
		assert_eq!(loudest(0, 0, 0..800), 0);
		assert!(loudest(0, 2, 0..800) < loudest(0, 4, 0..800));
	}
}
