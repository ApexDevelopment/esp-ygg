//! Waveshare ESP32-C6-Touch-LCD-2.8: an ST7789 screen of 240x320 on SPI,
//! turned on its side, a microSD slot on the same SPI bus, and a CST3530
//! touch panel on I2C. The screen's and the touch panel's reset lines and
//! the backlight hang off a CH32V003 on the same I2C bus, which Waveshare
//! programs to act as an I/O expander.
//!
//! The registers come from Waveshare's examples for the board.

use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO6, GPIO7, I2C0};
use esp_hal::time::Rate;
use log::warn;

#[cfg(feature = "ui")]
pub use screen::*;

pub const MODEL_NAME: &str = "Waveshare ESP32-C6-Touch-LCD-2.8";

const EXPANDER: u8 = 0x24;
const EXPANDER_BACKLIGHT: u8 = 0x05;

/// Turns the backlight off, for builds without an interface, where the
/// screen would otherwise sit lit and blank.
#[cfg(not(feature = "ui"))]
pub fn screen_off(i2c: I2C0<'static>, sda: GPIO6<'static>, scl: GPIO7<'static>) {
	expander_write(&mut i2c_bus(i2c, sda, scl), EXPANDER_BACKLIGHT, 0);
}

/// The bus the touch panel, the I/O expander and the board's sensors share.
fn i2c_bus(i2c: I2C0<'static>, sda: GPIO6<'static>, scl: GPIO7<'static>) -> I2c<'static, Blocking> {
	I2c::new(
		i2c,
		esp_hal::i2c::master::Config::default().with_frequency(Rate::from_khz(400)),
	)
	.expect("configuring the I2C bus")
	.with_sda(sda)
	.with_scl(scl)
}

fn expander_write(i2c: &mut I2c<'static, Blocking>, register: u8, value: u8) {
	if let Err(error) = i2c.write(EXPANDER, &[register, value]) {
		warn!("Writing to the I/O expander failed: {error:?}");
	}
}

/// The screen, the touch panel and the SD card, for the interfaces.
#[cfg(feature = "ui")]
mod screen {
	use core::cell::RefCell;

	use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};
	use embedded_graphics_core::prelude::DrawTarget;
	use embedded_hal::delay::DelayNs;
	use embedded_hal::spi::{ErrorType, Operation, SpiBus, SpiDevice};
	use esp_hal::Blocking;
	use esp_hal::delay::Delay;
	use esp_hal::gpio::{InputConfig, Level, Output, OutputConfig, Pull};
	use esp_hal::i2c::master::I2c;
	use esp_hal::peripherals::{
		DMA_CH0, GPIO0, GPIO1, GPIO6, GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO23, I2C0, SPI2,
	};
	use esp_hal::spi::Mode;
	use esp_hal::spi::master::{Config, ConfigError, Spi, SpiDma};
	use esp_hal::time::Rate;
	use esp_ygg_app::ui::touch::Point;
	use log::{info, warn};
	use mipidsi::interface::{Interface, SpiInterface};
	use mipidsi::models::ST7789;
	use mipidsi::options::{ColorInversion, Orientation, Rotation};
	use static_cell::StaticCell;

	use super::{EXPANDER_BACKLIGHT, MODEL_NAME, expander_write, i2c_bus};
	use crate::sdcard::{Slot, SpiPort};
	#[cfg(feature = "tui")]
	pub use text::start;

	const EXPANDER_DIRECTION: u8 = 0x02;
	const EXPANDER_OUTPUT: u8 = 0x03;
	const TOUCH_RESET: u8 = 1 << 0;
	const SCREEN_RESET: u8 = 1 << 1;
	/// Turns the speaker's amplifier on; left off.
	const AMPLIFIER: u8 = 1 << 3;

	const TOUCH: u8 = 0x58;
	/// The touch panel's registers have 32-bit addresses.
	const TOUCH_POINTS: u32 = 0xD007_0000;
	const TOUCH_DONE: u32 = 0xD000_02AB;

	/// The panel's own width standing upright, which is the screen's height
	/// as it is turned.
	const PANEL_WIDTH: u16 = 240;

	pub type Display = mipidsi::Display<
		SpiInterface<'static, ScreenSpi, Output<'static>>,
		ST7789,
		mipidsi::NoResetPin,
	>;

	/// The SPI bus the screen and the SD card share, driven by DMA. The
	/// graphical interface borrows the driver itself while it sends pixels.
	pub struct SharedSpi(Option<SpiDma<'static, Blocking>>);

	impl SharedSpi {
		fn driver(&mut self) -> &mut SpiDma<'static, Blocking> {
			self.0.as_mut().expect("the SPI driver was not handed back")
		}

		#[cfg(feature = "gui")]
		fn lend(&mut self) -> SpiDma<'static, Blocking> {
			self.0.take().expect("the SPI driver was not handed back")
		}

		#[cfg(feature = "gui")]
		fn hand_back(&mut self, driver: SpiDma<'static, Blocking>) {
			self.0 = Some(driver);
		}
	}

	impl ErrorType for SharedSpi {
		type Error = esp_hal::spi::Error;
	}

	impl SpiBus for SharedSpi {
		fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
			SpiBus::read(self.driver(), words)
		}

		fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
			SpiBus::write(self.driver(), words)
		}

		fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
			SpiBus::transfer(self.driver(), read, write)
		}

		fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
			SpiBus::transfer_in_place(self.driver(), words)
		}

		fn flush(&mut self) -> Result<(), Self::Error> {
			SpiBus::flush(self.driver())
		}
	}

	impl SpiPort for SharedSpi {
		fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError> {
			self.driver().apply_config(config)
		}
	}

	/// The screen's end of the shared bus.
	pub struct ScreenSpi {
		bus: &'static RefCell<SharedSpi>,
		cs: Output<'static>,
	}

	impl ErrorType for ScreenSpi {
		type Error = esp_hal::spi::Error;
	}

	impl SpiDevice for ScreenSpi {
		fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
			let mut bus = self.bus.borrow_mut();
			self.cs.set_low();
			let result = operations
				.iter_mut()
				.try_for_each(|operation| match operation {
					Operation::Read(words) => bus.read(words),
					Operation::Write(words) => bus.write(words),
					Operation::Transfer(read, write) => bus.transfer(read, write),
					Operation::TransferInPlace(words) => bus.transfer_in_place(words),
					Operation::DelayNs(nanoseconds) => {
						Delay::new().delay_ns(*nanoseconds);
						Ok(())
					}
				});
			self.cs.set_high();
			result
		}
	}

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
		pub dma: DMA_CH0<'static>,
	}

	pub fn model_name() -> &'static str {
		MODEL_NAME
	}

	/// The board's parts, ready for an interface to use. The graphical
	/// interface does not use the button yet.
	#[cfg_attr(feature = "gui", allow(dead_code))]
	pub struct Board {
		/// Cleared to black, with the backlight on.
		pub display: Display,
		/// The touch panel's bus.
		pub i2c: I2c<'static, Blocking>,
		pub boot_button: esp_hal::gpio::Input<'static>,
		pub slot: Slot,
	}

	/// Sets up the screen, the touch panel and the SD card slot.
	pub fn set_up(pins: Pins) -> Board {
		info!("Board: {MODEL_NAME}");
		let mut i2c = i2c_bus(pins.i2c, pins.sda, pins.scl);
		reset_screen_and_touch(&mut i2c);
		let driver = Spi::new(pins.spi, screen_speed())
			.expect("configuring the SPI bus")
			.with_sck(pins.sck)
			.with_mosi(pins.mosi)
			.with_miso(pins.sd_miso)
			.with_dma(pins.dma);
		static BUS: StaticCell<RefCell<SharedSpi>> = StaticCell::new();
		let bus = BUS.init(RefCell::new(SharedSpi(Some(driver))));
		// Keeps the SD card off the bus while the screen talks.
		let sd_cs = Output::new(pins.sd_cs, Level::High, OutputConfig::default());
		let display = display(bus, pins.cs, pins.dc);
		set_backlight(&mut i2c, u8::MAX);
		Board {
			display,
			i2c,
			boot_button: esp_hal::gpio::Input::new(
				pins.boot_button,
				InputConfig::default().with_pull(Pull::Up),
			),
			slot: Slot::new(bus, sd_cs, Some(screen_speed())),
		}
	}

	fn screen_speed() -> Config {
		Config::default()
			.with_frequency(Rate::from_mhz(80))
			.with_mode(Mode::_0)
			// Commands are quicker to send without setting up DMA.
			.with_min_async_transfer_size(64)
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

	/// Waveshare's settings for this panel's voltages, timing and gamma, from
	/// their examples. With the controller's own defaults, the panel holds a
	/// faint copy of whatever it showed for a while. Their list also sets
	/// how the controller orders bytes, which is left as the driver set it.
	const PANEL_SETTINGS: &[(u8, &[u8])] = &[
		// Porch timing
		(0xB2, &[0x0C, 0x0C, 0x00, 0x33, 0x33]),
		// Gate voltages
		(0xB7, &[0x75]),
		// The common voltage the liquid crystal is driven around
		(0xBB, &[0x1A]),
		(0xC0, &[0x2C]),
		(0xC2, &[0x01, 0xFF]),
		// Source voltages
		(0xC3, &[0x13]),
		(0xC4, &[0x20]),
		// 60 frames a second
		(0xC6, &[0x0F]),
		(0xD0, &[0xA4, 0xA1]),
		(0xD6, &[0xA1]),
		// Gamma, for light and dark
		(
			0xE0,
			&[
				0xD0, 0x0D, 0x14, 0x0D, 0x0D, 0x09, 0x38, 0x44, 0x4E, 0x3A, 0x17, 0x18, 0x2F, 0x30,
			],
		),
		(
			0xE1,
			&[
				0xD0, 0x09, 0x0F, 0x08, 0x07, 0x14, 0x37, 0x44, 0x4D, 0x38, 0x15, 0x16, 0x2C, 0x2E,
			],
		),
	];

	/// Starts the display, cleared to black.
	fn display(
		bus: &'static RefCell<SharedSpi>,
		cs: GPIO11<'static>,
		dc: GPIO10<'static>,
	) -> Display {
		let cs = Output::new(cs, Level::High, OutputConfig::default());
		let device = ScreenSpi { bus, cs };
		static BUFFER: StaticCell<[u8; 1024]> = StaticCell::new();
		let dc = Output::new(dc, Level::Low, OutputConfig::default());
		let interface = SpiInterface::new(device, dc, BUFFER.init([0; 1024]));
		let mut display = mipidsi::Builder::new(ST7789, interface)
			.orientation(Orientation::new().rotate(Rotation::Deg90))
			.invert_colors(ColorInversion::Inverted)
			.init(&mut Delay::new())
			.expect("starting the display");
		// SAFETY: these settings leave alone the orientation, pixel format
		// and byte order the driver set up and relies on.
		let commands = unsafe { display.dcs() };
		for &(command, arguments) in PANEL_SETTINGS {
			if let Err(error) = commands.send_command(command, arguments) {
				warn!("Setting up the panel failed: {error:?}");
			}
		}
		// The panel powers up with random contents, and interfaces only draw
		// what changes.
		display.clear(Rgb565::BLACK).expect("clearing the display");
		display
	}

	/// Sets the backlight, from off at 0 to full at 255.
	pub fn set_backlight(i2c: &mut I2c<'static, Blocking>, level: u8) {
		expander_write(i2c, EXPANDER_BACKLIGHT, level);
	}

	/// Where the panel is touched, if anywhere, in the turned screen's pixels.
	pub fn read_touch(i2c: &mut I2c<'static, Blocking>) -> Option<Point> {
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

	#[cfg(feature = "gui")]
	pub use panel::{BATCH_PIXELS, Panel};

	/// The screen driven directly, for the graphical interface, which draws
	/// a few lines at a time. Each batch goes out by DMA while the next is
	/// drawn.
	#[cfg(feature = "gui")]
	mod panel {
		use core::cell::RefCell;

		use esp_hal::Blocking;
		use esp_hal::dma::DmaTxBuf;
		use esp_hal::dma_tx_buffer;
		use esp_hal::gpio::Output;
		use esp_hal::spi::master::{SpiDma, SpiDmaTransfer};
		use log::warn;

		use super::{Display, ScreenSpi, SharedSpi};

		/// The most pixels sent at once.
		pub const BATCH_PIXELS: usize = 320 * 8;

		const DISPLAY_OFF: u8 = 0x28;
		const DISPLAY_ON: u8 = 0x29;
		const COLUMNS: u8 = 0x2A;
		const ROWS: u8 = 0x2B;
		const WRITE_MEMORY: u8 = 0x2C;

		pub struct Panel {
			bus: &'static RefCell<SharedSpi>,
			cs: Output<'static>,
			dc: Output<'static>,
			drawing: DmaTxBuf,
			/// The other buffer, while it is not being sent.
			spare: Option<DmaTxBuf>,
			sending: Option<SpiDmaTransfer<'static, Blocking, DmaTxBuf>>,
		}

		impl Panel {
			/// Takes over the screen, set up and cleared as `display` left it.
			pub fn new(display: Display) -> Self {
				let (interface, _, _) = display.release();
				let (ScreenSpi { bus, cs }, dc) = interface.release();
				let first = dma_tx_buffer!(BATCH_PIXELS * 2).expect("a pixel buffer");
				let second = dma_tx_buffer!(BATCH_PIXELS * 2).expect("a pixel buffer");
				Panel {
					bus,
					cs,
					dc,
					drawing: first,
					spare: Some(second),
					sending: None,
				}
			}

			/// Where to draw the next pixels, in RGB565.
			pub fn pixels(&mut self) -> &mut [u16] {
				// SAFETY: any two bytes make a u16, and DMA buffers are word
				// aligned, so nothing is left over at the start.
				let (start, pixels, _) =
					unsafe { self.drawing.as_mut_slice().align_to_mut::<u16>() };
				debug_assert!(start.is_empty());
				pixels
			}

			/// Puts the high byte of each of the first `count` pixels first,
			/// as the panel takes them, two pixels at a time.
			fn swap_bytes(&mut self, count: usize) {
				let bytes = &mut self.drawing.as_mut_slice()[..count.next_multiple_of(2) * 2];
				// SAFETY: any four bytes make a u32, and DMA buffers are word
				// aligned, so nothing is left over at the start.
				let (start, words, _) = unsafe { bytes.align_to_mut::<u32>() };
				debug_assert!(start.is_empty());
				for word in words {
					*word = (*word & 0x00FF_00FF) << 8 | (*word >> 8) & 0x00FF_00FF;
				}
			}

			/// Starts sending the first `count` pixels drawn, in rows from
			/// left to right, to the columns and rows given, and moves on to
			/// the other buffer.
			pub fn send(&mut self, columns: (u16, u16), rows: (u16, u16), count: usize) {
				self.swap_bytes(count);
				let mut driver = self.wait();
				self.cs.set_low();
				let [left, right] = [columns.0, columns.1].map(u16::to_be_bytes);
				let [top, bottom] = [rows.0, rows.1].map(u16::to_be_bytes);
				self.command(
					&mut driver,
					COLUMNS,
					&[left[0], left[1], right[0], right[1]],
				);
				self.command(&mut driver, ROWS, &[top[0], top[1], bottom[0], bottom[1]]);
				self.command(&mut driver, WRITE_MEMORY, &[]);
				let next = self.spare.take().expect("a spare pixel buffer");
				let mut batch = core::mem::replace(&mut self.drawing, next);
				batch.set_length(count * 2);
				match driver.write_buffer(count * 2, batch) {
					Ok(transfer) => self.sending = Some(transfer),
					Err((error, driver, batch)) => {
						warn!("Sending pixels failed: {error:?}");
						self.spare = Some(batch);
						self.release(driver);
					}
				}
			}

			/// Waits for the pixels on their way, and frees the bus. Until
			/// then, the last pixels go out while the next frame is worked
			/// out.
			pub fn finish(&mut self) {
				if self.sending.is_some() {
					let driver = self.wait();
					self.release(driver);
				}
			}

			/// Turns the picture on or off. Drawing still works while it is
			/// off, and turning it on shows what was drawn.
			pub fn show_picture(&mut self, on: bool) {
				let mut driver = self.wait();
				self.cs.set_low();
				self.command(&mut driver, if on { DISPLAY_ON } else { DISPLAY_OFF }, &[]);
				self.release(driver);
			}

			/// The SPI driver, once the pixels on their way have gone.
			fn wait(&mut self) -> SpiDma<'static, Blocking> {
				match self.sending.take() {
					Some(transfer) => {
						let (driver, batch) = transfer.wait();
						self.spare = Some(batch);
						driver
					}
					None => self.bus.borrow_mut().lend(),
				}
			}

			fn release(&mut self, driver: SpiDma<'static, Blocking>) {
				self.cs.set_high();
				self.bus.borrow_mut().hand_back(driver);
			}

			fn command(
				&mut self,
				driver: &mut SpiDma<'static, Blocking>,
				command: u8,
				arguments: &[u8],
			) {
				self.dc.set_low();
				let mut result = driver.write(&[command]);
				self.dc.set_high();
				if result.is_ok() && !arguments.is_empty() {
					result = driver.write(arguments);
				}
				if let Err(error) = result {
					warn!("Sending a command to the screen failed: {error:?}");
				}
			}
		}
	}

	/// The text interface's screen, and touch read as gestures for it.
	#[cfg(feature = "tui")]
	mod text {
		use embassy_time::{Duration, Timer};
		use esp_hal::Blocking;
		use esp_hal::i2c::master::I2c;
		use esp_ygg_app::ui::screen_timeout::ScreenTimeout;
		use esp_ygg_app::ui::touch::{Gesture, Gestures, Point};
		use mousefood::embedded_graphics::mono_font::MonoFont;
		use mousefood::{EmbeddedBackend, EmbeddedBackendConfig, TerminalAlignment};
		use ratatui::Terminal;
		use static_cell::StaticCell;

		use super::{Display, PANEL_WIDTH, Pins, read_touch, set_backlight, set_up};
		use crate::sdcard::Slot;
		use crate::ui::{INPUT, Input, Screen, uptime};

		const TOUCH_INTERVAL: Duration = Duration::from_millis(20);

		/// Starts the text interface's screen and the touch task, and returns the
		/// screen and the SD card slot.
		pub fn start(spawner: &embassy_executor::Spawner, pins: Pins) -> (Screen, Slot) {
			let board = set_up(pins);
			// 40 columns, as on the Cardputer, in a font that suits the larger screen.
			let font = mousefood::fonts::mono_8x13_atlas();
			let grid = Grid::new(&font);
			let screen = screen(board.display, font);
			spawner
				.spawn(touch(board.i2c, board.boot_button, grid).expect("spawning the touch task"));
			(screen, board.slot)
		}

		fn screen(display: Display, font: MonoFont<'static>) -> Screen {
			static DISPLAY: StaticCell<Display> = StaticCell::new();
			// The lines of text sit on the bottom edge, as the keyboard does, and
			// what is left over goes above them, as [`Grid`] expects.
			let config = EmbeddedBackendConfig {
				font_regular: font,
				vertical_alignment: TerminalAlignment::End,
				horizontal_alignment: TerminalAlignment::Start,
				..Default::default()
			};
			let backend = EmbeddedBackend::new(DISPLAY.init(display), config);
			Terminal::new(backend).expect("starting the terminal")
		}

		/// The character cells the screen is laid out in, which touches are
		/// counted in.
		#[derive(Clone, Copy)]
		struct Grid {
			width: u16,
			height: u16,
			/// The pixels above the first line, left over as the screen's height
			/// is not a whole number of lines.
			top: u16,
		}

		impl Grid {
			fn new(font: &MonoFont<'static>) -> Self {
				let height = font.character_size.height as u16;
				Grid {
					width: font.character_size.width as u16,
					height,
					top: PANEL_WIDTH % height,
				}
			}

			/// The column and row of the cell at `point`.
			fn cell(self, (x, y): Point) -> (u16, u16) {
				(x / self.width, y.saturating_sub(self.top) / self.height)
			}
		}

		/// Reads the touch panel and the BOOT button, which locks the board,
		/// and dims the screen when it goes untouched for a while.
		#[embassy_executor::task]
		async fn touch(
			mut i2c: I2c<'static, Blocking>,
			boot_button: esp_hal::gpio::Input<'static>,
			grid: Grid,
		) {
			let mut gestures = Gestures::new();
			let mut lock_button = crate::lock::Button::default();
			let mut timeout = ScreenTimeout::new(uptime());
			let mut backlight = u8::MAX;
			loop {
				lock_button.update(boot_button.is_low());
				timeout.set_locked(uptime(), crate::lock::locked_at(), false);
				let point = timeout.touch(uptime(), read_touch(&mut i2c));
				for gesture in gestures.update(point) {
					let input = match gesture {
						Gesture::Key(key) => Input::Key(key),
						Gesture::Tap(point) => {
							let (column, row) = grid.cell(point);
							Input::Tap { column, row }
						}
						Gesture::Press(point) => {
							let (column, row) = grid.cell(point);
							Input::Press { column, row }
						}
						Gesture::Release => Input::Release,
					};
					let _ = INPUT.try_send(input);
				}
				let level = timeout.backlight(uptime());
				if level != backlight {
					set_backlight(&mut i2c, level);
					backlight = level;
				}
				Timer::after(TOUCH_INTERVAL).await;
			}
		}
	}
}
