//! Waveshare ESP32-C6-Touch-LCD-2.8: an ST7789 screen of 240x320 on SPI,
//! turned on its side, a microSD slot on the same SPI bus, and a CST3530
//! touch panel on I2C. The screen's and
//! the touch panel's reset lines and the backlight hang off a CH32V003 on
//! the same I2C bus, which Waveshare programs to act as an I/O expander.
//!
//! The registers come from Waveshare's examples for the board.

use core::cell::RefCell;

use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::{NoDelay, RefCellDevice};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{
	GPIO0, GPIO1, GPIO6, GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO23, I2C0, SPI2,
};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_ygg_app::ui::Key;
use esp_ygg_app::ui::touch::{Gesture, Gestures, Point};
use log::{info, warn};
use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7789;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mousefood::embedded_graphics::mono_font::MonoFont;
use mousefood::embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use mousefood::embedded_graphics::prelude::DrawTarget;
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig, TerminalAlignment};
use ratatui::Terminal;
use static_cell::StaticCell;

use crate::sdcard::{Bus, Slot};
use crate::ui::{INPUT, Input, Screen};

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
		RefCellDevice<'static, Spi<'static, Blocking>, Output<'static>, NoDelay>,
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
	pub sd_miso: GPIO8<'static>,
	pub i2c: I2C0<'static>,
	pub sda: GPIO6<'static>,
	pub scl: GPIO7<'static>,
	pub boot_button: GPIO9<'static>,
}

pub fn model_name() -> &'static str {
	MODEL_NAME
}

/// Starts the screen and the touch task, and returns the screen and the
/// SD card slot.
pub fn start(spawner: &embassy_executor::Spawner, pins: Pins) -> (Screen, Slot) {
	info!("Board: {MODEL_NAME}");
	let mut i2c = I2c::new(
		pins.i2c,
		esp_hal::i2c::master::Config::default().with_frequency(Rate::from_khz(400)),
	)
	.expect("configuring the I2C bus")
	.with_sda(pins.sda)
	.with_scl(pins.scl);
	reset_screen_and_touch(&mut i2c);
	let bus = Spi::new(pins.spi, screen_speed())
		.expect("configuring the SPI bus")
		.with_sck(pins.sck)
		.with_mosi(pins.mosi)
		.with_miso(pins.sd_miso);
	static BUS: StaticCell<Bus> = StaticCell::new();
	let bus = BUS.init(RefCell::new(bus));
	// Keeps the SD card off the bus while the screen talks.
	let sd_cs = Output::new(pins.sd_cs, Level::High, OutputConfig::default());
	// 40 columns, as on the Cardputer, in a font that suits the larger screen.
	let font = mousefood::fonts::mono_8x13_atlas();
	let cell = (
		font.character_size.width as u16,
		font.character_size.height as u16,
	);
	let screen = screen(bus, pins.cs, pins.dc, font);
	expander_write(&mut i2c, EXPANDER_BACKLIGHT, u8::MAX);
	let boot_button =
		esp_hal::gpio::Input::new(pins.boot_button, InputConfig::default().with_pull(Pull::Up));
	spawner.spawn(touch(i2c, boot_button, cell).expect("spawning the touch task"));
	(screen, Slot::new(bus, sd_cs, Some(screen_speed())))
}

fn screen_speed() -> Config {
	Config::default()
		.with_frequency(Rate::from_mhz(80))
		.with_mode(Mode::_0)
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
	bus: &'static Bus,
	cs: GPIO11<'static>,
	dc: GPIO10<'static>,
	font: MonoFont<'static>,
) -> Screen {
	let cs = Output::new(cs, Level::High, OutputConfig::default());
	let device = RefCellDevice::new_no_delay(bus, cs).expect("display chip select");
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
	// The text starts at the top left, so taps map straight onto it.
	let config = EmbeddedBackendConfig {
		font_regular: font,
		vertical_alignment: TerminalAlignment::Start,
		horizontal_alignment: TerminalAlignment::Start,
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

/// Reads the touch panel and the BOOT button. `cell` is the size of a
/// character on the screen, which taps are counted in.
#[embassy_executor::task]
async fn touch(
	mut i2c: I2c<'static, Blocking>,
	boot_button: esp_hal::gpio::Input<'static>,
	cell: (u16, u16),
) {
	let (cell_width, cell_height) = cell;
	let mut gestures = Gestures::new();
	let mut button_held = false;
	loop {
		let point = read_touch(&mut i2c);
		for gesture in gestures.update(point) {
			let input = match gesture {
				Gesture::Key(key) => Input::Key(key),
				Gesture::Tap((x, y)) => Input::Tap {
					column: x / cell_width,
					row: y / cell_height,
				},
			};
			let _ = INPUT.try_send(input);
		}
		// The BOOT button goes back, as a swipe to the right does.
		if boot_button.is_low() && !button_held {
			let _ = INPUT.try_send(Input::Key(Key::Escape));
		}
		button_held = boot_button.is_low();
		Timer::after(TOUCH_INTERVAL).await;
	}
}
