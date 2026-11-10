//! The speaker and microphone. Both Cardputers drive them over one I2S bus,
//! one direction at a time, which suits push-to-talk. The ADV routes both
//! through an ES8311 codec, set up over the keyboard's I2C bus.
//!
//! For now, holding the G0 button records, and letting go plays it back.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::dma::aligned::DmaAlignedMut;
use esp_hal::dma::{DmaDescriptor, DmaRxStreamBuf, DmaTxStreamBuf};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::peripherals::{DMA_CH0, GPIO0, GPIO41, GPIO42, GPIO43, GPIO46, I2S0};
use esp_hal::time::Rate;
use esp_ygg_app::voice::{FRAME_SAMPLES, SAMPLE_RATE, mulaw};
use log::{info, warn};

use crate::cardputer::Model;

/// Register writes for the ES8311 codec, carried out by the keyboard task,
/// which owns the I2C bus.
pub static CODEC_WRITES: Channel<CriticalSectionRawMutex, (u8, u8), 16> = Channel::new();
pub const CODEC: u8 = 0x18;
/// Set while the serial port holds push-to-talk down.
pub static REMOTE_TALK: AtomicBool = AtomicBool::new(false);

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
	// Microphone input at the lowest gain, 0 dB, without the DC offset
	(0x14, 0x10),
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

/// The pins the I2S bus uses, which recording and playing take in turn.
struct Bus {
	i2s: I2S0<'static>,
	dma: DMA_CH0<'static>,
	bclk: GPIO41<'static>,
	dout: GPIO42<'static>,
	ws: GPIO43<'static>,
	din: GPIO46<'static>,
}

struct Audio {
	pins: Bus,
	model: Model,
	button: Input<'static>,
	rx_buffer: Option<DmaRxStreamBuf>,
	tx_buffer: Option<DmaTxStreamBuf>,
}

#[embassy_executor::task]
pub async fn run(pins: Pins, model: Model) {
	let button = Input::new(pins.button, InputConfig::default().with_pull(Pull::Up));
	let bus = Bus {
		i2s: pins.i2s,
		dma: pins.dma,
		bclk: pins.bclk,
		dout: pins.dout,
		ws: pins.ws,
		din: pins.din,
	};
	if model == Model::Adv {
		for &write in CODEC_SETUP {
			CODEC_WRITES.send(write).await;
		}
	}
	let mut audio = Audio {
		pins: bus,
		model,
		button,
		rx_buffer: Some(
			DmaRxStreamBuf::new(descriptors(8), dma_memory(8 * FRAME_BYTES))
				.expect("making the microphone's buffer"),
		),
		tx_buffer: Some(
			DmaTxStreamBuf::new(descriptors(6), dma_memory(6 * FRAME_BYTES))
				.expect("making the speaker's buffer"),
		),
	};
	loop {
		while !audio.talking() {
			Timer::after(BUTTON_POLL).await;
		}
		let recording = audio.record().await;
		audio.play(&recording).await;
	}
}

impl Audio {
	fn talking(&self) -> bool {
		self.button.is_low() || REMOTE_TALK.load(Ordering::Relaxed)
	}

	fn config() -> TdmConfig {
		TdmConfig::new_tdm_philips()
			.with_sample_rate(Rate::from_hz(SAMPLE_RATE))
			.with_data_format(DataFormat::Data16Channel16)
			.with_channels(Channels::STEREO)
	}

	/// Records while push-to-talk is held, as μ-law.
	async fn record(&mut self) -> Vec<u8> {
		let mut recording = Vec::new();
		if self.model != Model::Adv {
			warn!("Recording needs the ADV's microphone for now");
			while self.talking() {
				Timer::after(BUTTON_POLL).await;
			}
			return recording;
		}
		let pins = &mut self.pins;
		let i2s = I2s::new(pins.i2s.reborrow(), pins.dma.reborrow(), Self::config())
			.expect("configuring I2S")
			.into_async();
		let rx = i2s
			.i2s_rx
			.with_bclk(pins.bclk.reborrow())
			.with_ws(pins.ws.reborrow())
			.with_din(pins.din.reborrow())
			.build();
		let buffer = self
			.rx_buffer
			.take()
			.expect("returned after each recording");
		let mut transfer = match rx.read(buffer) {
			Ok(transfer) => transfer,
			Err((error, _, buffer)) => {
				warn!("Starting the microphone failed: {error:?}");
				self.rx_buffer = Some(buffer);
				return recording;
			}
		};
		let started = Instant::now();
		let mut armings = 0;
		let mut chunk = [0u8; FRAME_BYTES];
		let mut levels = [Level::default(); 2];
		let limit = SAMPLE_RATE as usize * LONGEST_RECORDING.as_secs() as usize;
		'recording: while self.button.is_low() || REMOTE_TALK.load(Ordering::Relaxed) {
			if armings < 3 && started.elapsed() >= Duration::from_millis(30 * (armings + 1)) {
				let _ = CODEC_WRITES.try_send(CODEC_ARM);
				armings += 1;
			}
			if let Err(error) = transfer.wait_for_available_async().await {
				warn!("The microphone stopped: {error:?}");
				break;
			}
			// Take everything waiting, or the DMA runs out of room.
			loop {
				let length = transfer.pop(&mut chunk) / 4 * 4;
				if length == 0 {
					break;
				}
				for frame in chunk[..length].chunks_exact(4) {
					for (slot, level) in levels.iter_mut().enumerate() {
						level.add(i16::from_le_bytes([frame[2 * slot], frame[2 * slot + 1]]));
					}
					let at = 2 * MICROPHONE_SLOT;
					let sample = i16::from_le_bytes([frame[at], frame[at + 1]]);
					if recording.len() >= limit {
						break 'recording;
					}
					recording.push(mulaw::encode(sample));
				}
			}
		}
		let (_, buffer) = transfer.stop();
		self.rx_buffer = Some(buffer);
		info!(
			"Recorded {} ms; left {}, right {}",
			recording.len() as u32 * 1000 / SAMPLE_RATE,
			levels[0],
			levels[1],
		);
		recording
	}

	/// Plays μ-law audio to the end.
	async fn play(&mut self, audio: &[u8]) {
		if audio.is_empty() {
			return;
		}
		let pins = &mut self.pins;
		let i2s = I2s::new(pins.i2s.reborrow(), pins.dma.reborrow(), Self::config())
			.expect("configuring I2S")
			.into_async();
		let tx = i2s
			.i2s_tx
			.with_bclk(pins.bclk.reborrow())
			.with_ws(pins.ws.reborrow())
			.with_dout(pins.dout.reborrow())
			.build();
		let mut buffer = self.tx_buffer.take().expect("returned after playing");
		let mut samples = audio
			.iter()
			.map(|&byte| mulaw::decode(byte))
			.chain(core::iter::repeat_n(0, 2 * FRAME_SAMPLES));
		let mut fill = |space: &mut [u8]| {
			let mut written = 0;
			for frame in space.chunks_exact_mut(4) {
				let Some(sample) = samples.next() else { break };
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
		loop {
			if transfer.wait_for_available_async().await.is_err() {
				break;
			}
			if transfer.push_with(&mut fill) == 0 {
				// Everything is queued; let the buffer play out.
				Timer::after(Duration::from_millis(150)).await;
				break;
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
