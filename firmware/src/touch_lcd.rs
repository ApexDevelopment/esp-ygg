//! Waveshare ESP32-C6-Touch-LCD-2.8: an ST7789 screen of 240x320 on SPI,
//! turned on its side, and a CST3530 touch panel on I2C. The screen's and
//! the touch panel's reset lines and the backlight hang off a CH32V003 on
//! the same I2C bus, which Waveshare programs to act as an I/O expander.
//!
//! The registers come from Waveshare's examples for the board.

use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO0, GPIO1, GPIO6, GPIO7, GPIO9, GPIO10, GPIO11, GPIO23, I2C0, SPI2};
use esp_hal::spi::Mode;
use esp_hal::spi::master::Spi;
use esp_hal::time::Rate;
use esp_ygg_app::ui::Key;
use esp_ygg_app::ui::touch::{Gestures, Point};
use log::{info, warn};
use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7789;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mousefood::embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use mousefood::embedded_graphics::prelude::DrawTarget;
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
use ratatui::Terminal;
use static_cell::StaticCell;

use crate::ui::{KEYS, Screen};

pub const MODEL_NAME: &str = "Waveshare ESP32-C6-Touch-LCD-2.8";

const EXPANDER: u8 = 0x24;
const EXPANDER_DIRECTION: u8 = 0x02;
const EXPANDER_OUTPUT: u8 = 0x03;
const EXPANDER_BACKLIGHT: u8 = 0x05;
const TOUCH_RESET: u8 = 1 << 0;
const SCREEN_RESET: u8 = 1 << 1;
/// Turns the speaker's amplifier on; left off.
const AMPLIFIER: u8 = 1 << 3;

const TOUCH: u8 = 0x58;
/// The touch panel's registers have 32-bit addresses.
const TOUCH_POINTS: u32 = 0xD007_0000;
const TOUCH_DONE: u32 = 0xD000_02AB;
const TOUCH_INTERVAL: Duration = Duration::from_millis(20);

/// The panel's own size, standing upright.
const PANEL_WIDTH: u16 = 240;

pub type Display = mipidsi::Display<
	SpiInterface<
		'static,
		ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, NoDelay>,
		Output<'static>,
	>,
	ST7789,
	mipidsi::NoResetPin,
>;

pub struct Pins {
	pub spi: SPI2<'static>,
	pub sck: GPIO0<'static>,
	pub mosi: GPIO1<'static>,
	pub cs: GPIO11<'static>,
	pub dc: GPIO10<'static>,
	/// The SD card shares the screen's SPI bus.
	pub sd_cs: GPIO23<'static>,
	pub i2c: I2C0<'static>,
	pub sda: GPIO6<'static>,
	pub scl: GPIO7<'static>,
	pub boot_button: GPIO9<'static>,
}

pub fn model_name() -> &'static str {
	MODEL_NAME
}

/// Starts the screen and the touch task.
pub fn start(spawner: &embassy_executor::Spawner, pins: Pins) -> Screen {
	info!("Board: {MODEL_NAME}");
	let mut i2c = I2c::new(
		pins.i2c,
		esp_hal::i2c::master::Config::default().with_frequency(Rate::from_khz(400)),
	)
	.expect("configuring the I2C bus")
	.with_sda(pins.sda)
	.with_scl(pins.scl);
	reset_screen_and_touch(&mut i2c);
	// Keeps the SD card off the bus while the screen talks.
	let _sd_cs = Output::new(pins.sd_cs, Level::High, OutputConfig::default());
	let screen = screen(pins.spi, pins.sck, pins.mosi, pins.cs, pins.dc);
	expander_write(&mut i2c, EXPANDER_BACKLIGHT, u8::MAX);
	let boot_button = Input::new(pins.boot_button, InputConfig::default().with_pull(Pull::Up));
	spawner.spawn(touch(i2c, boot_button).expect("spawning the touch task"));
	screen
}

/// Holds both reset lines low, then lets them go and waits for the touch
/// panel, the slower of the two, to wake.
fn reset_screen_and_touch(i2c: &mut I2c<'static, Blocking>) {
	expander_write(i2c, EXPANDER_OUTPUT, 0);
	expander_write(
		i2c,
		EXPANDER_DIRECTION,
		TOUCH_RESET | SCREEN_RESET | AMPLIFIER,
	);
	Delay::new().delay_millis(100);
	expander_write(i2c, EXPANDER_OUTPUT, TOUCH_RESET | SCREEN_RESET);
	Delay::new().delay_millis(500);
}

fn expander_write(i2c: &mut I2c<'static, Blocking>, register: u8, value: u8) {
	if let Err(error) = i2c.write(EXPANDER, &[register, value]) {
		warn!("Writing to the I/O expander failed: {error:?}");
	}
}

fn screen(
	spi: SPI2<'static>,
	sck: GPIO0<'static>,
	mosi: GPIO1<'static>,
	cs: GPIO11<'static>,
	dc: GPIO10<'static>,
) -> Screen {
	let config = esp_hal::spi::master::Config::default()
		.with_frequency(Rate::from_mhz(80))
		.with_mode(Mode::_0);
	let spi = Spi::new(spi, config)
		.expect("configuring the display SPI bus")
		.with_sck(sck)
		.with_mosi(mosi);
	let cs = Output::new(cs, Level::High, OutputConfig::default());
	let device = ExclusiveDevice::new_no_delay(spi, cs).expect("display chip select");
	static BUFFER: StaticCell<[u8; 1024]> = StaticCell::new();
	let dc = Output::new(dc, Level::Low, OutputConfig::default());
	let interface = SpiInterface::new(device, dc, BUFFER.init([0; 1024]));
	let mut display = mipidsi::Builder::new(ST7789, interface)
		.orientation(Orientation::new().rotate(Rotation::Deg90))
		.invert_colors(ColorInversion::Inverted)
		.init(&mut Delay::new())
		.expect("starting the display");
	// The panel powers up with random contents, and the terminal only draws
	// cells that change, so blank cells would never be drawn over.
	display.clear(Rgb565::BLACK).expect("clearing the display");
	static DISPLAY: StaticCell<Display> = StaticCell::new();
	// 40 columns, as on the Cardputer, in a font that suits the larger screen.
	let config = EmbeddedBackendConfig {
		font_regular: mousefood::fonts::mono_8x13_atlas(),
		..Default::default()
	};
	let backend = EmbeddedBackend::new(DISPLAY.init(display), config);
	Terminal::new(backend).expect("starting the terminal")
}

/// Where the panel is touched, if anywhere, in the turned screen's pixels.
fn read_touch(i2c: &mut I2c<'static, Blocking>) -> Option<Point> {
	let mut data = [0u8; 9];
	let read = i2c.write_read(TOUCH, &TOUCH_POINTS.to_be_bytes(), &mut data);
	let _ = i2c.write(TOUCH, &TOUCH_DONE.to_be_bytes());
	if let Err(error) = read {
		warn!("Reading the touch panel failed: {error:?}");
		return None;
	}
	if data[3] & 0x0F == 0 || data[8] & 0xF0 == 0 {
		return None;
	}
	let x = u16::from(data[7] & 0x0F) << 8 | u16::from(data[4]);
	let y = u16::from(data[7] & 0xF0) << 4 | u16::from(data[5]);
	Some((y, PANEL_WIDTH.saturating_sub(1 + x)))
}

#[embassy_executor::task]
async fn touch(mut i2c: I2c<'static, Blocking>, boot_button: Input<'static>) {
	let mut gestures = Gestures::new();
	let mut button_held = false;
	loop {
		let point = read_touch(&mut i2c);
		for key in gestures.update(point) {
			let _ = KEYS.try_send(key);
		}
		// The BOOT button goes back, as a swipe to the right does.
		if boot_button.is_low() && !button_held {
			let _ = KEYS.try_send(Key::Escape);
		}
		button_held = boot_button.is_low();
		Timer::after(TOUCH_INTERVAL).await;
	}
}
