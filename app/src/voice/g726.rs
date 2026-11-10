//! G.726 ADPCM at 32 kbit/s: 4 bits per sample. The arithmetic follows the
//! ITU recommendation, where widths and wrapping matter for a decoder to
//! match others bit for bit.

const POWER2: [i32; 15] = [
	1, 2, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000,
];
/// Quantizer decision levels
const QUANTIZER: [i32; 7] = [-124, 80, 178, 246, 300, 349, 400];
/// Log of the reconstructed difference, for each code
const DQLN: [i32; 16] = [
	-2048, 4, 135, 213, 273, 323, 373, 425, 425, 373, 323, 273, 213, 135, 4, -2048,
];
/// Scale factor multipliers
const WI: [i32; 16] = [
	-12, 18, 41, 64, 112, 198, 355, 1122, 1122, 355, 198, 112, 64, 41, 18, -12,
];
/// Rate of change of the adaptation speed
const FI: [i32; 16] = [
	0, 0, 0, 0x200, 0x200, 0x200, 0x600, 0xe00, 0xe00, 0x600, 0x200, 0x200, 0x200, 0, 0, 0,
];

/// One direction of a G.726 stream. Encoders and decoders keep the same
/// state, so each side of a call needs its own.
#[derive(Clone, Debug)]
pub struct State {
	yl: i32,
	yu: i32,
	dms: i32,
	dml: i32,
	ap: i32,
	a: [i32; 2],
	b: [i32; 6],
	pk: [i32; 2],
	dq: [i32; 6],
	sr: [i32; 2],
	td: bool,
}

impl Default for State {
	fn default() -> Self {
		State {
			yl: 34816,
			yu: 544,
			dms: 0,
			dml: 0,
			ap: 0,
			a: [0; 2],
			b: [0; 6],
			pk: [0; 2],
			dq: [32; 6],
			sr: [32; 2],
			td: false,
		}
	}
}

impl State {
	/// Encodes one 16-bit sample as a 4-bit code.
	pub fn encode(&mut self, sample: i16) -> u8 {
		let sl = i32::from(sample) >> 2;
		let (se, sez, y) = self.predict();
		let difference = sl - se;
		let code = quantize(difference, y);
		self.adapt(code, se, sez, y);
		code as u8
	}

	/// Decodes one 4-bit code into a 16-bit sample.
	pub fn decode(&mut self, code: u8) -> i16 {
		let code = i32::from(code & 0x0f);
		let (se, sez, y) = self.predict();
		let sr = self.adapt(code, se, sez, y);
		(sr << 2) as i16
	}

	/// The estimated signal, its zero-predictor part, and the step size.
	fn predict(&self) -> (i32, i32, i32) {
		let sezi = (0..6)
			.map(|index| fmult(self.b[index] >> 2, self.dq[index]))
			.sum::<i32>();
		let pole = fmult(self.a[1] >> 2, self.sr[1]) + fmult(self.a[0] >> 2, self.sr[0]);
		((sezi + pole) >> 1, sezi >> 1, self.step_size())
	}

	/// Reconstructs the signal for `code` and updates the state. Returns
	/// the reconstructed signal, in 14 bits.
	fn adapt(&mut self, code: i32, se: i32, sez: i32, y: i32) -> i32 {
		let index = code as usize;
		let dq = reconstruct(code & 8 != 0, DQLN[index], y);
		let sr = if dq < 0 { se - (dq & 0x3fff) } else { se + dq };
		let dqsez = sr + sez - se;
		self.update(y, WI[index] << 5, FI[index], dq, sr, dqsez);
		sr
	}

	fn step_size(&self) -> i32 {
		if self.ap >= 256 {
			return self.yu;
		}
		let mut y = self.yl >> 6;
		let difference = self.yu - y;
		let speed = self.ap >> 2;
		if difference > 0 {
			y += (difference * speed) >> 6;
		} else if difference < 0 {
			y += (difference * speed + 0x3f) >> 6;
		}
		y
	}

	fn update(&mut self, y: i32, wi: i32, fi: i32, dq: i32, sr: i32, dqsez: i32) {
		let pk0 = i32::from(dqsez < 0);
		let magnitude = dq & 0x7fff;

		// Tone and transition detection
		let ylint = self.yl >> 15;
		let ylfrac = (self.yl >> 10) & 0x1f;
		let thr1 = (32 + ylfrac) << ylint;
		let thr2 = if ylint > 9 { 31 << 10 } else { thr1 };
		let dqthr = (thr2 + (thr2 >> 1)) >> 1;
		let transition = self.td && magnitude > dqthr;

		// Quantizer scale factor
		self.yu = (y + ((wi - y) >> 5)).clamp(544, 5120);
		self.yl += self.yu + ((-self.yl) >> 6);

		// Predictor coefficients
		let mut a2p = 0;
		if transition {
			self.a = [0; 2];
			self.b = [0; 6];
		} else {
			let pks1 = pk0 ^ self.pk[0];
			a2p = self.a[1] - (self.a[1] >> 7);
			if dqsez != 0 {
				let fa1 = if pks1 != 0 { self.a[0] } else { -self.a[0] };
				if fa1 < -8191 {
					a2p -= 0x100;
				} else if fa1 > 8191 {
					a2p += 0xff;
				} else {
					a2p += fa1 >> 5;
				}
				if pk0 ^ self.pk[1] != 0 {
					if a2p <= -12160 {
						a2p = -12288;
					} else if a2p >= 12416 {
						a2p = 12288;
					} else {
						a2p -= 0x80;
					}
				} else if a2p <= -12416 {
					a2p = -12288;
				} else if a2p >= 12160 {
					a2p = 12288;
				} else {
					a2p += 0x80;
				}
			}
			self.a[1] = a2p;

			self.a[0] -= self.a[0] >> 8;
			if dqsez != 0 {
				if pks1 == 0 {
					self.a[0] += 192;
				} else {
					self.a[0] -= 192;
				}
			}
			let a1ul = 15360 - a2p;
			self.a[0] = self.a[0].clamp(-a1ul, a1ul);

			for (b, &previous) in self.b.iter_mut().zip(&self.dq) {
				*b -= *b >> 8;
				if magnitude != 0 {
					// The stored differences are 16-bit, so their sign is bit 15.
					if (dq ^ sign_extend(previous)) >= 0 {
						*b += 128;
					} else {
						*b -= 128;
					}
				}
			}
		}

		self.dq.copy_within(0..5, 1);
		self.dq[0] = if magnitude == 0 {
			if dq >= 0 { 0x20 } else { 0xfc20 }
		} else {
			let exponent = quan(magnitude);
			let float = (exponent << 6) + ((magnitude << 6) >> exponent);
			if dq >= 0 { float } else { float - 0x400 }
		};

		self.sr[1] = self.sr[0];
		self.sr[0] = if sr == 0 {
			0x20
		} else if sr > 0 {
			let exponent = quan(sr);
			(exponent << 6) + ((sr << 6) >> exponent)
		} else if sr > -32768 {
			let magnitude = -sr;
			let exponent = quan(magnitude);
			(exponent << 6) + ((magnitude << 6) >> exponent) - 0x400
		} else {
			0xfc20
		};

		self.pk[1] = self.pk[0];
		self.pk[0] = pk0;

		self.td = !transition && a2p < -11776;

		// Adaptation speed
		self.dms += (fi - self.dms) >> 5;
		self.dml += ((fi << 2) - self.dml) >> 7;
		self.ap += if transition {
			256 - self.ap
		} else if y < 1536 || self.td || ((self.dms << 2) - self.dml).abs() >= (self.dml >> 3) {
			(0x200 - self.ap) >> 4
		} else {
			(-self.ap) >> 4
		};
	}
}

/// The values the recommendation keeps in 16 bits, read back as signed.
fn sign_extend(value: i32) -> i32 {
	i32::from(value as u16 as i16)
}

/// How many of the powers of two `value` reaches.
fn quan(value: i32) -> i32 {
	POWER2.iter().take_while(|&&power| value >= power).count() as i32
}

/// Multiplies a predictor coefficient by a signal in the recommendation's
/// small floating-point format.
fn fmult(an: i32, srn: i32) -> i32 {
	let srn = sign_extend(srn);
	let magnitude = if an > 0 { an } else { (-an) & 0x1fff };
	let exponent = quan(magnitude) - 6;
	let mantissa = if magnitude == 0 {
		32
	} else if exponent >= 0 {
		magnitude >> exponent
	} else {
		magnitude << -exponent
	};
	let product_exponent = exponent + ((srn >> 6) & 0xf) - 13;
	let product_mantissa = (mantissa * (srn & 0o77) + 0x30) >> 4;
	let product = if product_exponent >= 0 {
		(product_mantissa << product_exponent) & 0x7fff
	} else {
		product_mantissa >> -product_exponent
	};
	if (an ^ srn) < 0 { -product } else { product }
}

fn quantize(difference: i32, y: i32) -> i32 {
	let magnitude = difference.abs();
	let exponent = quan(magnitude >> 1);
	let mantissa = ((magnitude << 7) >> exponent) & 0x7f;
	let log = (exponent << 7) + mantissa;
	let normalized = log - (y >> 2);
	let level = QUANTIZER
		.iter()
		.take_while(|&&decision| normalized >= decision)
		.count() as i32;
	if difference < 0 {
		15 - level
	} else if level == 0 {
		15
	} else {
		level
	}
}

fn reconstruct(negative: bool, dqln: i32, y: i32) -> i32 {
	let dql = dqln + (y >> 2);
	if dql < 0 {
		return if negative { -0x8000 } else { 0 };
	}
	let exponent = (dql >> 7) & 15;
	let mantissa = 128 + (dql & 127);
	let dq = (mantissa << 7) >> (14 - exponent);
	if negative { dq - 0x8000 } else { dq }
}

/// Packs codes two to a byte, the first in the low four bits, as RFC 3551
/// and Asterisk's IAX2 format 0x10 do.
pub fn encode(state: &mut State, samples: &[i16]) -> alloc::vec::Vec<u8> {
	samples
		.chunks(2)
		.map(|pair| {
			let low = state.encode(pair[0]);
			let high = pair.get(1).map_or(0, |&sample| state.encode(sample));
			low | high << 4
		})
		.collect()
}

pub fn decode(state: &mut State, data: &[u8]) -> alloc::vec::Vec<i16> {
	data.iter()
		.flat_map(|&byte| [byte & 0x0f, byte >> 4])
		.map(|code| state.decode(code))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec::Vec;

	/// A signal of ramps, noise and silence, in integers so it is the same
	/// everywhere.
	fn signal() -> Vec<i16> {
		let mut seed = 12345u32;
		(0..4000i32)
			.map(|n| {
				seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
				let noise = ((seed >> 16) % 2001) as i32 - 1000;
				let slow = (n * 37 % 400 - 200) * 60;
				let fast = (n * 523 % 72 - 36) * 150;
				let envelope = (n % 1000).min(1000 - n % 1000) * 2;
				((slow + fast) * envelope / 1000 + noise).clamp(-32768, 32767) as i16
			})
			.collect()
	}

	fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
		bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
			(hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
		})
	}

	/// The expected values come from Sun's G.721 reference code, which
	/// Asterisk's G.726 codec is built on.
	#[test]
	fn matches_the_reference_bit_for_bit() {
		let encoded = encode(&mut State::default(), &signal());
		assert_eq!(
			encoded[..8],
			[0x88, 0x78, 0x87, 0xe9, 0x18, 0x83, 0xe1, 0xd3]
		);
		assert_eq!(fnv(encoded.iter().copied()), 0x8d5ec9b1a024ff07);
		let decoded = decode(&mut State::default(), &encoded);
		let bytes = decoded.iter().flat_map(|sample| sample.to_le_bytes());
		assert_eq!(fnv(bytes), 0xd634ab09212b9724);
	}
}
