//! Voice calls: codecs and the audio that moves between them and the
//! board's speaker and microphone.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub mod g726;
pub mod iax2;
pub mod mulaw;
pub mod ringtone;

/// The codecs this build offers in calls, most preferred first.
pub const CODECS: &[Codec] = &[Codec::G726, Codec::G726Aal2, Codec::Ulaw];

/// Samples per second, the telephone rate every codec here uses.
pub const SAMPLE_RATE: u32 = 8000;
/// Samples in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 160;

/// The codecs calls can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
	/// G.711 μ-law, 64 kbit/s
	Ulaw,
	/// G.726 at 32 kbit/s, packed as RFC 3551 has it
	G726,
	/// G.726 at 32 kbit/s, packed as ATM's AAL2 has it
	G726Aal2,
}

impl Codec {
	/// The IAX2 format bit. These follow Asterisk, which swapped the two
	/// G.726 bits from what RFC 5456 lists, and which other software follows.
	pub fn format(self) -> u32 {
		match self {
			Codec::Ulaw => 0x04,
			Codec::G726Aal2 => 0x10,
			Codec::G726 => 0x800,
		}
	}

	pub fn from_format(format: u32) -> Option<Self> {
		match format {
			0x04 => Some(Codec::Ulaw),
			0x10 => Some(Codec::G726Aal2),
			0x800 => Some(Codec::G726),
			_ => None,
		}
	}

	fn packing(self) -> g726::Packing {
		match self {
			Codec::G726Aal2 => g726::Packing::Aal2,
			_ => g726::Packing::Rfc3551,
		}
	}
}

/// Turns samples into a codec's bytes, one stream at a time.
pub struct Encoder {
	codec: Codec,
	g726: g726::State,
}

impl Encoder {
	pub fn new(codec: Codec) -> Self {
		Encoder {
			codec,
			g726: g726::State::default(),
		}
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
			Codec::G726 | Codec::G726Aal2 => {
				g726::encode(&mut self.g726, self.codec.packing(), samples)
			}
		}
	}
}

/// Turns a codec's bytes back into samples, one stream at a time.
pub struct Decoder {
	codec: Codec,
	g726: g726::State,
}

impl Decoder {
	pub fn new(codec: Codec) -> Self {
		Decoder {
			codec,
			g726: g726::State::default(),
		}
	}

	pub fn codec(&self) -> Codec {
		self.codec
	}

	pub fn decode(&mut self, data: &[u8]) -> Vec<i16> {
		match self.codec {
			Codec::Ulaw => data.iter().map(|&byte| mulaw::decode(byte)).collect(),
			Codec::G726 | Codec::G726Aal2 => {
				g726::decode(&mut self.g726, self.codec.packing(), data)
			}
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

/// Removes a steady offset from samples, such as a microphone without its
/// own high-pass filter has, keeping the voice band.
#[derive(Default)]
pub struct DcBlocker {
	/// None until the first sample, which sets the offset to remove.
	previous_input: Option<i32>,
	/// The output, scaled up by 2^15 to keep precision.
	previous_output: i64,
}

impl DcBlocker {
	pub fn filter(&mut self, sample: i16) -> i16 {
		// y[n] = x[n] - x[n-1] + 0.995 y[n-1], with a cut-off near 6 Hz
		const POLE: i64 = 32604;
		let input = i32::from(sample);
		let previous = self.previous_input.unwrap_or(input);
		let output = i64::from(input - previous) * 32768 + self.previous_output * POLE / 32768;
		self.previous_input = Some(input);
		self.previous_output = output;
		(output / 32768).clamp(i16::MIN.into(), i16::MAX.into()) as i16
	}
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
		let played: Vec<i16> = (0..3 * FRAME_SAMPLES)
			.map(|_| buffer.next_sample())
			.collect();
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
	fn dc_blocker_removes_an_offset_and_keeps_a_tone() {
		let mut blocker = DcBlocker::default();
		let tone = |index: usize| if index % 20 < 10 { 1000 } else { -1000 };
		let output: Vec<i16> = (0..8000)
			.map(|index| blocker.filter(1500 + tone(index)))
			.collect();
		let settled = &output[4000..];
		let mean =
			settled.iter().map(|&sample| i64::from(sample)).sum::<i64>() / settled.len() as i64;
		assert!(mean.abs() < 20, "offset left: {mean}");
		let peak = settled.iter().map(|sample| sample.abs()).max().unwrap();
		assert!(peak > 900, "tone lost: {peak}");
	}

	#[test]
	fn dc_blocker_starts_without_a_click() {
		let mut blocker = DcBlocker::default();
		assert!((0..100).all(|_| blocker.filter(1500) == 0));
	}
}
