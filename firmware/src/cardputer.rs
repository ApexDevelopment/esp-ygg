//! M5Stack Cardputer and Cardputer ADV: the screen and the keyboard. Both
//! share the screen and key layout; the original scans its keys through a
//! 74HC138 decoder, and the ADV reads them from a TCA8418 over I2C.

use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{
	GPIO3, GPIO4, GPIO5, GPIO6, GPIO7, GPIO8, GPIO9, GPIO11, GPIO13, GPIO15, GPIO33, GPIO34,
	GPIO35, GPIO36, GPIO37, GPIO38, I2C0, SPI2,
};
use esp_hal::spi::Mode;
use esp_hal::spi::master::Spi;
use esp_hal::time::Rate;
use esp_ygg_app::ui::Key;
use esp_ygg_app::ui::keyboard::{Keyboard, Position};
use log::info;
use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7789;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mousefood::embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use mousefood::embedded_graphics::prelude::DrawTarget;
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
use ratatui::Terminal;
use static_cell::StaticCell;

/// Key presses, ready for the UI.
pub static KEYS: Channel<CriticalSectionRawMutex, Key, 16> = Channel::new();

const KEYBOARD_CONTROLLER: u8 = 0x34;
const SCAN_INTERVAL: Duration = Duration::from_millis(10);

type Display = mipidsi::Display<
	SpiInterface<
		'static,
		ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, NoDelay>,
		Output<'static>,
	>,
	ST7789,
	Output<'static>,
>;
pub type Screen = Terminal<EmbeddedBackend<'static, Display, Rgb565>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
	Original,
	Adv,
}

impl Model {
	pub fn name(self) -> &'static str {
		match self {
			Model::Original => "M5Stack Cardputer",
			Model::Adv => "M5Stack Cardputer ADV",
		}
	}
}

pub struct Pins {
	pub spi: SPI2<'static>,
	pub sck: GPIO36<'static>,
	pub mosi: GPIO35<'static>,
	pub cs: GPIO37<'static>,
	pub dc: GPIO34<'static>,
	pub reset: GPIO33<'static>,
	pub backlight: GPIO38<'static>,
	pub i2c: I2C0<'static>,
	pub gpio8: GPIO8<'static>,
	pub gpio9: GPIO9<'static>,
	pub gpio11: GPIO11<'static>,
	pub gpio13: GPIO13<'static>,
	pub gpio15: GPIO15<'static>,
	pub gpio3: GPIO3<'static>,
	pub gpio4: GPIO4<'static>,
	pub gpio5: GPIO5<'static>,
	pub gpio6: GPIO6<'static>,
	pub gpio7: GPIO7<'static>,
}

/// Starts the screen and the keyboard task, and returns which model this is.
pub fn start(spawner: &embassy_executor::Spawner, mut pins: Pins) -> (Model, Screen) {
	let model = if has_keyboard_controller(&mut pins) {
		Model::Adv
	} else {
		Model::Original
	};
	info!("Board: {}", model.name());
	let screen = screen(
		pins.spi,
		pins.sck,
		pins.mosi,
		pins.cs,
		pins.dc,
		pins.reset,
		pins.backlight,
	);
	let reader = match model {
		Model::Adv => KeyReader::controller(pins.i2c, pins.gpio8, pins.gpio9),
		Model::Original => KeyReader::Matrix {
			select: [output(pins.gpio8), output(pins.gpio9), output(pins.gpio11)],
			read: [
				input(pins.gpio13),
				input(pins.gpio15),
				input(pins.gpio3),
				input(pins.gpio4),
				input(pins.gpio5),
				input(pins.gpio6),
				input(pins.gpio7),
			],
		},
	};
	spawner.spawn(keyboard(reader).expect("spawning the keyboard task"));
	(model, screen)
}

fn output(pin: impl esp_hal::gpio::OutputPin + 'static) -> Output<'static> {
	Output::new(pin, Level::Low, OutputConfig::default())
}

fn input(pin: impl esp_hal::gpio::InputPin + 'static) -> Input<'static> {
	Input::new(pin, InputConfig::default().with_pull(Pull::Up))
}

/// Whether the ADV's keyboard controller answers on I2C. On the original,
/// these pins drive the decoder, and the probe only toggles its inputs.
fn has_keyboard_controller(pins: &mut Pins) -> bool {
	let Ok(i2c) = I2c::new(pins.i2c.reborrow(), i2c_config()) else {
		return false;
	};
	let mut i2c = i2c
		.with_sda(pins.gpio8.reborrow())
		.with_scl(pins.gpio9.reborrow());
	let mut config = [0u8];
	i2c.write_read(KEYBOARD_CONTROLLER, &[REG_CFG], &mut config)
		.is_ok()
}

fn i2c_config() -> esp_hal::i2c::master::Config {
	esp_hal::i2c::master::Config::default().with_frequency(Rate::from_khz(400))
}

fn screen(
	spi: SPI2<'static>,
	sck: GPIO36<'static>,
	mosi: GPIO35<'static>,
	cs: GPIO37<'static>,
	dc: GPIO34<'static>,
	reset: GPIO33<'static>,
	backlight: GPIO38<'static>,
) -> Screen {
	let config = esp_hal::spi::master::Config::default()
		.with_frequency(Rate::from_mhz(40))
		.with_mode(Mode::_0);
	let spi = Spi::new(spi, config)
		.expect("configuring the display SPI bus")
		.with_sck(sck)
		.with_mosi(mosi);
	let device = ExclusiveDevice::new_no_delay(spi, output(cs)).expect("display chip select");
	static BUFFER: StaticCell<[u8; 512]> = StaticCell::new();
	let interface = SpiInterface::new(device, output(dc), BUFFER.init([0; 512]));
	// The panel is 135x240, mounted sideways at an offset in the
	// controller's memory, with inverted colors, as M5Stack's M5GFX sets it up.
	let mut display = mipidsi::Builder::new(ST7789, interface)
		.display_size(135, 240)
		.display_offset(52, 40)
		.orientation(Orientation::new().rotate(Rotation::Deg90))
		.invert_colors(ColorInversion::Inverted)
		.reset_pin(output(reset))
		.init(&mut Delay::new())
		.expect("starting the display");
	// The panel powers up with random contents, and the terminal only draws
	// cells that change, so blank cells would never be drawn over.
	display.clear(Rgb565::BLACK).expect("clearing the display");
	// The pin stays high once the driver is dropped. On the ADV, it also
	// powers the RGB LED.
	let _backlight = Output::new(backlight, Level::High, OutputConfig::default());
	static DISPLAY: StaticCell<Display> = StaticCell::new();
	let backend = EmbeddedBackend::new(DISPLAY.init(display), EmbeddedBackendConfig::default());
	Terminal::new(backend).expect("starting the terminal")
}

enum KeyReader {
	/// Three pins select one of eight lines on the decoder; seven read keys.
	Matrix {
		select: [Output<'static>; 3],
		read: [Input<'static>; 7],
	},
	Controller {
		i2c: I2c<'static, Blocking>,
		held: Vec<Position>,
	},
}

const REG_CFG: u8 = 0x01;
const REG_INT_STAT: u8 = 0x02;
const REG_KEY_LCK_EC: u8 = 0x03;
const REG_KEY_EVENT_A: u8 = 0x04;
const REG_KP_GPIO_1: u8 = 0x1D;
const REG_KP_GPIO_2: u8 = 0x1E;
const CFG_KE_IEN: u8 = 0x01;

impl KeyReader {
	fn controller(i2c: I2C0<'static>, sda: GPIO8<'static>, scl: GPIO9<'static>) -> Self {
		let mut i2c = I2c::new(i2c, i2c_config())
			.expect("configuring the keyboard I2C bus")
			.with_sda(sda)
			.with_scl(scl);
		// Seven rows and eight columns of keys, with key events counted.
		for (register, value) in [
			(REG_KP_GPIO_1, 0x7F),
			(REG_KP_GPIO_2, 0xFF),
			(REG_CFG, CFG_KE_IEN),
		] {
			let _ = i2c.write(KEYBOARD_CONTROLLER, &[register, value]);
		}
		// Drop anything pressed before now.
		read_events(&mut i2c, &mut Vec::new());
		KeyReader::Controller {
			i2c,
			held: Vec::new(),
		}
	}

	/// The keys held right now.
	fn held(&mut self) -> Vec<Position> {
		match self {
			KeyReader::Matrix { select, read } => scan_matrix(select, read),
			KeyReader::Controller { i2c, held } => {
				read_events(i2c, held);
				held.clone()
			}
		}
	}
}

fn scan_matrix(select: &mut [Output<'static>; 3], read: &[Input<'static>; 7]) -> Vec<Position> {
	let mut held = Vec::new();
	for line in 0..8u8 {
		for (bit, pin) in select.iter_mut().enumerate() {
			pin.set_level(Level::from(line & (1 << bit) != 0));
		}
		Delay::new().delay_micros(2);
		for (input, pin) in read.iter().enumerate() {
			if pin.is_low() {
				let input = input as u8;
				let column = if line > 3 { 2 * input } else { 2 * input + 1 };
				held.push((3 - line % 4, column));
			}
		}
	}
	held
}

fn read_events(i2c: &mut I2c<'static, Blocking>, held: &mut Vec<Position>) {
	let mut count = [0u8];
	if i2c
		.write_read(KEYBOARD_CONTROLLER, &[REG_KEY_LCK_EC], &mut count)
		.is_err()
	{
		return;
	}
	for _ in 0..(count[0] & 0x0F) {
		let mut event = [0u8];
		if i2c
			.write_read(KEYBOARD_CONTROLLER, &[REG_KEY_EVENT_A], &mut event)
			.is_err() || event[0] == 0
		{
			break;
		}
		// Keys are numbered row * 10 + column + 1 on the controller, and
		// wired so that each controller row covers two layout columns.
		let Some(code) = (event[0] & 0x7F).checked_sub(1) else {
			continue;
		};
		let (row, column) = (code / 10, code % 10);
		let position = (column % 4, row * 2 + u8::from(column > 3));
		if event[0] & 0x80 != 0 {
			if !held.contains(&position) {
				held.push(position);
			}
		} else {
			held.retain(|&key| key != position);
		}
	}
	let _ = i2c.write(KEYBOARD_CONTROLLER, &[REG_INT_STAT, 0x03]);
}

#[embassy_executor::task]
async fn keyboard(mut reader: KeyReader) {
	let mut keyboard = Keyboard::new();
	loop {
		let held = reader.held();
		for key in keyboard.update(&held, crate::platform::now()) {
			let _ = KEYS.try_send(key);
		}
		Timer::after(SCAN_INTERVAL).await;
	}
}
