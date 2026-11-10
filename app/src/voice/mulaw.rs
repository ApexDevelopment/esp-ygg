//! G.711 μ-law: each 16-bit sample in one byte, on a logarithmic scale.

const BIAS: i32 = 0x84;
const CLIP: i32 = 32635;

pub fn encode(sample: i16) -> u8 {
	let mut magnitude = i32::from(sample);
	let sign = if magnitude < 0 {
		magnitude = -magnitude;
		0x80
	} else {
		0
	};
	let biased = magnitude.min(CLIP) + BIAS;
	let exponent = (31 - (biased as u32).leading_zeros()) - 7;
	let mantissa = (biased >> (exponent + 3)) & 0x0f;
	!(sign | (exponent << 4) as i32 | mantissa) as u8
}

pub fn decode(byte: u8) -> i16 {
	let byte = !byte;
	let exponent = (byte >> 4) & 0x07;
	let mantissa = i32::from(byte & 0x0f);
	let magnitude = (((mantissa << 3) + BIAS) << exponent) - BIAS;
	(if byte & 0x80 != 0 {
		-magnitude
	} else {
		magnitude
	}) as i16
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn matches_the_standard_at_the_ends() {
		assert_eq!(encode(0), 0xff);
		assert_eq!(decode(0xff), 0);
		assert_eq!(encode(i16::MAX), 0x80);
		assert_eq!(decode(0x80), 32124);
		assert_eq!(decode(0x00), -32124);
	}

	#[test]
	fn every_code_survives_a_round_trip() {
		// 0x7f is negative zero, which encodes as positive zero.
		for code in (0..=255u8).filter(|&code| code != 0x7f) {
			assert_eq!(encode(decode(code)), code, "code {code:#x}");
		}
	}

	#[test]
	fn keeps_samples_close() {
		for sample in (-32000..32000).step_by(7) {
			let error = (i32::from(decode(encode(sample as i16))) - sample).abs();
			assert!(
				error <= 4 + sample.abs() / 16,
				"{sample} came back off by {error}"
			);
		}
	}
}
