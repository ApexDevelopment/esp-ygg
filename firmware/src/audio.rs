//! The speaker and microphone. Both Cardputers drive them over one I2S bus,
//! one direction at a time, which suits push-to-talk. The ADV routes both
//! through an ES8311 codec, set up over the keyboard's I2C bus.
//!
//! In a call, the speaker plays what arrives until the G0 button is held,
//! and then the microphone sends instead. Outside a call, holding G0
//! records, and letting go plays it back, to check both work.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::dma::aligned::DmaAlignedMut;
use esp_hal::dma::{DmaDescriptor, DmaRxStreamBuf, DmaTxStreamBuf};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2s::master::{
	Channels, DataFormat, I2s, PdmConfig, PdmRxConfig, PdmSlotMode, TdmConfig,
};
use esp_hal::i2s::pdm::PdmSlotMask;
use esp_hal::peripherals::{DMA_CH0, GPIO0, GPIO41, GPIO42, GPIO43, GPIO46, I2S0};
use esp_hal::time::Rate;
use esp_ygg_app::voice::{
	Codec, DcBlocker, Decoder, Encoder, FRAME_SAMPLES, JitterBuffer, SAMPLE_RATE, mulaw, ring,
};
use log::{info, warn};

use crate::cardputer::Model;

/// Register writes for the ES8311 codec, carried out by the keyboard task,
/// which owns the I2C bus.
pub static CODEC_WRITES: Channel<CriticalSectionRawMutex, (u8, u8), 16> = Channel::new();
pub const CODEC: u8 = 0x18;
/// Set while the serial port holds push-to-talk down.
pub static REMOTE_TALK: AtomicBool = AtomicBool::new(false);
/// Set over the serial port to silence the speaker while testing.
pub static MUTED: AtomicBool = AtomicBool::new(false);
/// Set while push-to-talk is held, for the screen.
pub static TALKING: AtomicBool = AtomicBool::new(false);

/// Voice from the call, and voice for it, one 20 ms frame each.
pub static VOICE_IN: Channel<CriticalSectionRawMutex, (Codec, Vec<u8>), 16> = Channel::new();
pub static VOICE_OUT: Channel<CriticalSectionRawMutex, Vec<u8>, 8> = Channel::new();

/// The call as the network side sees it: none, ringing here, or up, with
/// the codec's place in [`CALL_CODECS`] after those.
static CALL: AtomicU8 = AtomicU8::new(NO_CALL);
const NO_CALL: u8 = 0;
const RINGING: u8 = 1;
const CALL_CODECS: [Codec; 3] = [Codec::Ulaw, Codec::G726, Codec::G726Aal2];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallState {
	None,
	Ringing,
	Up(Codec),
}

/// Called by the network side whenever the call changes.
pub fn set_call(ringing: bool, up: Option<Codec>) {
	let value = match (up, ringing) {
		(Some(codec), _) => {
			let index = CALL_CODECS.iter().position(|&known| known == codec);
			2 + index.expect("every codec is listed") as u8
		}
		(None, true) => RINGING,
		(None, false) => NO_CALL,
	};
	CALL.store(value, Ordering::Relaxed);
}

fn call_state() -> CallState {
	match CALL.load(Ordering::Relaxed) {
		NO_CALL => CallState::None,
		RINGING => CallState::Ringing,
		value => CALL_CODECS
			.get(usize::from(value - 2))
			.map_or(CallState::None, |&codec| CallState::Up(codec)),
	}
}

/// Each sample goes out in both slots, left and right, of 16 bits each.
const FRAME_BYTES: usize = FRAME_SAMPLES * 4;
/// The slot the codec puts the microphone in: the right, as M5Unified reads it.
const MICROPHONE_SLOT: usize = 1;
const LONGEST_RECORDING: Duration = Duration::from_secs(3);
const BUTTON_POLL: Duration = Duration::from_millis(10);

/// The ADV's codec with its ADC and DAC both on, as M5Stack's M5Unified
/// sets them up for the microphone and speaker in turn.
const CODEC_SETUP: &[(u8, u8)] = &[
	// Power on, with the master clock taken from the bit clock times 8
	(0x00, 0x80),
	(0x01, 0xbf),
	(0x02, 0x18),
	// 16-bit samples both ways
	(0x09, 0x0c),
	(0x0a, 0x0c),
	(0x0d, 0x01),
	(0x0e, 0x02),
	// DAC to the speaker driver, at 0 dB
	(0x12, 0x00),
	(0x13, 0x10),
	(0x32, 0xbf),
	(0x37, 0x08),
	// Microphone input with +18 dB of analog gain, ADC at 0 dB, without
	// the DC offset
	(0x14, 0x16),
	(0x17, 0xbf),
	(0x1c, 0x6a),
];
/// The codec only starts capturing once this is written with the bit
/// clock running, about 25 ms after it starts.
const CODEC_ARM: (u8, u8) = (0x0d, 0x01);

pub struct Pins {
	pub i2s: I2S0<'static>,
	pub dma: DMA_CH0<'static>,
	pub button: GPIO0<'static>,
	pub bclk: GPIO41<'static>,
	pub dout: GPIO42<'static>,
	pub ws: GPIO43<'static>,
	pub din: GPIO46<'static>,
}

/// The I2S bus, which the microphone and speaker take in turn, with a DMA
/// buffer for each.
struct Bus {
	i2s: I2S0<'static>,
	dma: DMA_CH0<'static>,
	bclk: GPIO41<'static>,
	dout: GPIO42<'static>,
	ws: GPIO43<'static>,
	din: GPIO46<'static>,
	model: Model,
	rx_buffer: Option<DmaRxStreamBuf>,
	tx_buffer: Option<DmaTxStreamBuf>,
}

#[embassy_executor::task]
pub async fn run(pins: Pins, model: Model) {
	let button = Input::new(pins.button, InputConfig::default().with_pull(Pull::Up));
	if model == Model::Adv {
		for &write in CODEC_SETUP {
			CODEC_WRITES.send(write).await;
		}
	}
	let mut bus = Bus {
		i2s: pins.i2s,
		dma: pins.dma,
		bclk: pins.bclk,
		dout: pins.dout,
		ws: pins.ws,
		din: pins.din,
		model,
		rx_buffer: Some(
			DmaRxStreamBuf::new(descriptors(8), dma_memory(8 * FRAME_BYTES))
				.expect("making the microphone's buffer"),
		),
		tx_buffer: Some(
			DmaTxStreamBuf::new(descriptors(6), dma_memory(6 * FRAME_BYTES))
				.expect("making the speaker's buffer"),
		),
	};
	let talking = || button.is_low() || REMOTE_TALK.load(Ordering::Relaxed);
	loop {
		TALKING.store(talking(), Ordering::Relaxed);
		match call_state() {
			CallState::Up(codec) if talking() => {
				let mut encoder = Encoder::new(codec);
				let mut frame = Vec::with_capacity(FRAME_SAMPLES);
				let keep = || talking() && call_state() == CallState::Up(codec);
				bus.capture(keep, |sample| {
					frame.push(sample);
					if frame.len() == FRAME_SAMPLES {
						let _ = VOICE_OUT.try_send(encoder.encode(&frame));
						frame.clear();
					}
					true
				})
				.await;
			}
			CallState::Up(codec) => {
				let mut decoder = Decoder::new(codec);
				let mut jitter = JitterBuffer::default();
				let keep = || !talking() && call_state() == CallState::Up(codec);
				bus.play(keep, || {
					while let Ok((codec, data)) = VOICE_IN.try_receive() {
						if codec != decoder.codec() {
							decoder = Decoder::new(codec);
						}
						jitter.push(&decoder.decode(&data));
					}
					Some(jitter.next_sample())
				})
				.await;
			}
			CallState::Ringing => {
				let mut index = 0;
				let keep = || call_state() == CallState::Ringing;
				bus.play(keep, || {
					index += 1;
					Some(ring(index))
				})
				.await;
			}
			CallState::None if talking() => {
				let mut recording = Vec::new();
				let limit = SAMPLE_RATE as usize * LONGEST_RECORDING.as_secs() as usize;
				let levels = bus
					.capture(talking, |sample| {
						recording.push(mulaw::encode(sample));
						recording.len() < limit
					})
					.await;
				info!(
					"Recorded {} ms, {levels}",
					recording.len() as u32 * 1000 / SAMPLE_RATE
				);
				let mut played = recording.iter().map(|&byte| mulaw::decode(byte));
				bus.play(|| call_state() == CallState::None, || played.next())
					.await;
			}
			CallState::None => {
				// Whatever arrives between calls has nowhere to go.
				while VOICE_IN.try_receive().is_ok() {}
				Timer::after(BUTTON_POLL).await;
			}
		}
	}
}

impl Bus {
	fn config() -> TdmConfig {
		TdmConfig::new_tdm_philips()
			.with_sample_rate(Rate::from_hz(SAMPLE_RATE))
			.with_data_format(DataFormat::Data16Channel16)
			.with_channels(Channels::STEREO)
	}

	/// The original Cardputer's SPM1423 microphone, in the right slot as
	/// M5Unified reads it.
	fn pdm_config() -> PdmRxConfig {
		let mut config =
			PdmRxConfig::new_pcm_default(Rate::from_hz(SAMPLE_RATE), PdmSlotMode::Mono);
		config.slot = config.slot.with_slot_mask(PdmSlotMask::RIGHT);
		config
	}

	/// Hands microphone samples to `sink` while `keep` holds and `sink`
	/// returns true. Returns how loud they were.
	async fn capture(
		&mut self,
		keep: impl Fn() -> bool,
		mut sink: impl FnMut(i16) -> bool,
	) -> Level {
		let mut level = Level::default();
		// The ADV's codec sends stereo frames; the original's PDM
		// microphone, converted in hardware, sends one sample at a time.
		let (rx, frame_bytes, at) = match self.model {
			Model::Adv => {
				let i2s = I2s::new(self.i2s.reborrow(), self.dma.reborrow(), Self::config())
					.expect("configuring I2S")
					.into_async();
				let rx = i2s
					.i2s_rx
					.with_bclk(self.bclk.reborrow())
					.with_ws(self.ws.reborrow())
					.with_din(self.din.reborrow())
					.build();
				(rx, 4, 2 * MICROPHONE_SLOT)
			}
			Model::Original => {
				let config = PdmConfig::rx_only(Self::pdm_config());
				let i2s = I2s::new_pdm(self.i2s.reborrow(), self.dma.reborrow(), config)
					.expect("configuring PDM")
					.into_async();
				let rx = i2s
					.i2s_rx
					.with_clk(self.ws.reborrow())
					.with_din_line(0, self.din.reborrow())
					.expect("PDM has a first data line")
					.build();
				(rx, 2, 0)
			}
		};
		let buffer = self.rx_buffer.take().expect("returned after each capture");
		let mut transfer = match rx.read(buffer) {
			Ok(transfer) => transfer,
			Err((error, _, buffer)) => {
				warn!("Starting the microphone failed: {error:?}");
				self.rx_buffer = Some(buffer);
				return level;
			}
		};
		let started = Instant::now();
		let mut armings = 0;
		let mut dc_blocker = DcBlocker::default();
		// The PDM converter needs a moment to settle after starting.
		let mut settling = if self.model == Model::Original {
			FRAME_SAMPLES
		} else {
			0
		};
		let mut chunk = [0u8; FRAME_BYTES];
		'capturing: while keep() {
			TALKING.store(true, Ordering::Relaxed);
			if self.model == Model::Adv
				&& armings < 3
				&& started.elapsed() >= Duration::from_millis(30 * (armings + 1))
			{
				let _ = CODEC_WRITES.try_send(CODEC_ARM);
				armings += 1;
			}
			if let Err(error) = transfer.wait_for_available_async().await {
				warn!("The microphone stopped: {error:?}");
				break;
			}
			// Take everything waiting, or the DMA runs out of room.
			loop {
				let length = transfer.pop(&mut chunk) / frame_bytes * frame_bytes;
				if length == 0 {
					break;
				}
				for frame in chunk[..length].chunks_exact(frame_bytes) {
					let sample = i16::from_le_bytes([frame[at], frame[at + 1]]);
					if settling > 0 {
						settling -= 1;
						continue;
					}
					let sample = dc_blocker.filter(sample);
					level.add(sample);
					if !sink(sample) {
						break 'capturing;
					}
				}
			}
		}
		let (_, buffer) = transfer.stop();
		self.rx_buffer = Some(buffer);
		level
	}

	/// Plays what `source` gives until it runs out, or until `keep` no
	/// longer holds, when it stops at once.
	async fn play(&mut self, keep: impl Fn() -> bool, mut source: impl FnMut() -> Option<i16>) {
		let i2s = I2s::new(self.i2s.reborrow(), self.dma.reborrow(), Self::config())
			.expect("configuring I2S")
			.into_async();
		let tx = i2s
			.i2s_tx
			.with_bclk(self.bclk.reborrow())
			.with_ws(self.ws.reborrow())
			.with_dout(self.dout.reborrow())
			.build();
		let mut buffer = self.tx_buffer.take().expect("returned after playing");
		let finished = core::cell::Cell::new(false);
		let mut fill = |space: &mut [u8]| {
			let mut written = 0;
			for frame in space.chunks_exact_mut(4) {
				let Some(sample) = (if finished.get() { None } else { source() }) else {
					finished.set(true);
					// Silence, so the end is not cut short
					frame.fill(0);
					written += 4;
					continue;
				};
				let sample = if MUTED.load(Ordering::Relaxed) {
					0
				} else {
					sample
				};
				frame[..2].copy_from_slice(&sample.to_le_bytes());
				frame[2..].copy_from_slice(&sample.to_le_bytes());
				written += 4;
			}
			written
		};
		while buffer.push_with(&mut fill) > 0 {}
		let mut transfer = match tx.write(buffer) {
			Ok(transfer) => transfer,
			Err((error, _, buffer)) => {
				warn!("Starting the speaker failed: {error:?}");
				self.tx_buffer = Some(buffer);
				return;
			}
		};
		let mut drained = 0;
		while keep() {
			if transfer.wait_for_available_async().await.is_err() {
				break;
			}
			transfer.push_with(&mut fill);
			// Once the source ends, let the queued audio play out.
			if finished.get() {
				drained += 1;
				if drained > 8 {
					break;
				}
			}
		}
		let (_, buffer) = transfer.stop();
		self.tx_buffer = Some(buffer);
	}
}

// DMA buffers come from the heap rather than statics, because the first
// core's stack is whatever RAM statics and the heap leave over.

fn descriptors(count: usize) -> DmaAlignedMut<'static, [DmaDescriptor]> {
	let descriptors = Vec::from_iter(core::iter::repeat_n(DmaDescriptor::EMPTY, count));
	DmaAlignedMut::new(Vec::leak(descriptors)).expect("descriptors are word aligned")
}

fn dma_memory(length: usize) -> DmaAlignedMut<'static, [u8]> {
	let layout = core::alloc::Layout::from_size_align(length, 4).expect("a valid layout");
	// SAFETY: the layout is not empty, and the memory is never freed.
	let memory = unsafe {
		let pointer = alloc::alloc::alloc_zeroed(layout);
		assert!(!pointer.is_null(), "out of memory for audio");
		core::slice::from_raw_parts_mut(pointer, length)
	};
	DmaAlignedMut::new(memory).expect("allocated word aligned")
}

/// How loud a stretch of samples was, for checking the microphone.
#[derive(Clone, Copy, Default)]
struct Level {
	peak: i32,
	sum_of_squares: u64,
	count: u64,
}

impl Level {
	fn add(&mut self, sample: i16) {
		self.peak = self.peak.max(i32::from(sample).abs());
		self.sum_of_squares += (i64::from(sample) * i64::from(sample)) as u64;
		self.count += 1;
	}
}

impl core::fmt::Display for Level {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		let rms = self
			.sum_of_squares
			.checked_div(self.count)
			.map_or(0, integer_sqrt);
		write!(f, "peak {} RMS {rms}", self.peak)
	}
}

fn integer_sqrt(value: u64) -> u64 {
	let mut root = 0u64;
	while (root + 1) * (root + 1) <= value {
		root += 1;
	}
	root
}
