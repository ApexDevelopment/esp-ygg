//! Voice calls: codecs and the audio that moves between them and the
//! board's speaker and microphone.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub mod iax2;
pub mod mulaw;

/// The codecs this build offers in calls, most preferred first.
pub const CODECS: &[Codec] = &[Codec::Ulaw];

/// Samples per second, the telephone rate every codec here uses.
pub const SAMPLE_RATE: u32 = 8000;
/// Samples in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 160;

/// The codecs calls can use, named by their IAX2 format bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
	/// G.711 μ-law, 64 kbit/s
	Ulaw,
	/// G.726 at 32 kbit/s
	G726,
}

impl Codec {
	pub fn format(self) -> u32 {
		match self {
			Codec::Ulaw => 0x04,
			Codec::G726 => 0x10,
		}
	}

	pub fn from_format(format: u32) -> Option<Self> {
		match format {
			0x04 => Some(Codec::Ulaw),
			0x10 => Some(Codec::G726),
			_ => None,
		}
	}
}

/// Turns samples into a codec's bytes, one stream at a time.
pub struct Encoder {
	codec: Codec,
}

impl Encoder {
	pub fn new(codec: Codec) -> Self {
		Encoder { codec }
	}

	pub fn codec(&self) -> Codec {
		self.codec
	}

	pub fn encode(&mut self, samples: &[i16]) -> Vec<u8> {
		match self.codec {
			Codec::Ulaw => samples
				.iter()
				.map(|&sample| mulaw::encode(sample))
				.collect(),
			// Not offered until it is built.
			Codec::G726 => Vec::new(),
		}
	}
}

/// Turns a codec's bytes back into samples, one stream at a time.
pub struct Decoder {
	codec: Codec,
}

impl Decoder {
	pub fn new(codec: Codec) -> Self {
		Decoder { codec }
	}

	pub fn codec(&self) -> Codec {
		self.codec
	}

	pub fn decode(&mut self, data: &[u8]) -> Vec<i16> {
		match self.codec {
			Codec::Ulaw => data.iter().map(|&byte| mulaw::decode(byte)).collect(),
			Codec::G726 => Vec::new(),
		}
	}
}

/// Holds received audio long enough to smooth out uneven arrival, and
/// plays silence while there is none.
#[derive(Default)]
pub struct JitterBuffer {
	samples: VecDeque<i16>,
	playing: bool,
}

/// Audio waits for this much before it starts playing.
const JITTER_START: usize = 3 * FRAME_SAMPLES;
/// Beyond this much, the oldest audio is dropped to catch up.
const JITTER_LIMIT: usize = 10 * FRAME_SAMPLES;

impl JitterBuffer {
	pub fn push(&mut self, samples: &[i16]) {
		self.samples.extend(samples);
		let excess = self.samples.len().saturating_sub(JITTER_LIMIT);
		self.samples.drain(..excess);
	}

	/// The next sample to play.
	pub fn next_sample(&mut self) -> i16 {
		if !self.playing && self.samples.len() >= JITTER_START {
			self.playing = true;
		}
		if !self.playing {
			return 0;
		}
		self.samples.pop_front().unwrap_or_else(|| {
			// Ran dry: wait to build up again.
			self.playing = false;
			0
		})
	}
}

/// A telephone-style ring: two short bursts, then a pause. Returns the
/// sample at `index` of the repeating pattern.
pub fn ring(index: usize) -> i16 {
	const ON: usize = SAMPLE_RATE as usize * 2 / 5;
	const OFF: usize = SAMPLE_RATE as usize / 5;
	const PATTERN: usize = 2 * (ON + OFF) + SAMPLE_RATE as usize * 2;
	let at = index % PATTERN;
	let sounding = at < ON || (ON + OFF..2 * ON + OFF).contains(&at);
	if !sounding {
		return 0;
	}
	// A 400 Hz triangle wave, gentle on a small speaker.
	const PERIOD: usize = SAMPLE_RATE as usize / 400;
	let phase = (at % PERIOD) as i32;
	let half = PERIOD as i32 / 2;
	let triangle = if phase < half {
		phase
	} else {
		PERIOD as i32 - phase
	};
	((triangle * 4 - half * 2) * 4000 / half) as i16
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn jitter_buffer_waits_then_plays_in_order() {
		let mut buffer = JitterBuffer::default();
		buffer.push(&[1; FRAME_SAMPLES]);
		assert_eq!(buffer.next_sample(), 0, "too little to start");
		buffer.push(&[2; FRAME_SAMPLES]);
		buffer.push(&[3; FRAME_SAMPLES]);
		let played: Vec<i16> = (0..3 * FRAME_SAMPLES).map(|_| buffer.next_sample()).collect();
		assert_eq!(played[0], 1);
		assert_eq!(played[3 * FRAME_SAMPLES - 1], 3);
		assert_eq!(buffer.next_sample(), 0, "silence once dry");
	}

	#[test]
	fn jitter_buffer_drops_the_oldest_when_too_far_behind() {
		let mut buffer = JitterBuffer::default();
		for frame in 0..20 {
			buffer.push(&[frame; FRAME_SAMPLES]);
		}
		assert_eq!(buffer.next_sample(), 10);
	}

	#[test]
	fn ring_sounds_then_pauses() {
		let loud = (0..800)
			.map(ring)
			.map(|sample| i32::from(sample).abs())
			.max();
		assert!(loud.unwrap() > 3000);
		let quiet_from = 2 * (3200 + 1600);
		assert!((quiet_from..quiet_from + 1000).all(|index| ring(index) == 0));
	}
}
