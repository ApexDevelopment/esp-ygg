//! Runs the graphical interface, made with Slint, on the touch board. Slint
//! draws a line at a time, so the screen needs no frame buffer.

use alloc::boxed::Box;
use alloc::rc::Rc;

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics_core::pixelcolor::Rgb565;
use embedded_graphics_core::pixelcolor::raw::RawU16;
use esp_ygg_app::Settings;
use esp_ygg_app::ui::Command;
use esp_ygg_app::ui::screen_timeout::ScreenTimeout;
use esp_ygg_gui::{Gui, slint};
use log::info;
use slint::ComponentHandle;
use slint::platform::software_renderer::{
	LineBufferProvider, MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};

use crate::touch_lcd::{self, Board, Display};
use crate::ui::{FROM_UI, INPUT, Input, TO_UI, ToUi, uptime};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
/// How often the touch panel is read, and the screen redrawn if it changed.
const FRAME: Duration = Duration::from_millis(20);
/// A draw slower than this is logged. Redrawing the whole screen takes
/// about 90 ms, and a page about 60.
const SLOW_DRAW: Duration = Duration::from_millis(100);

struct Esp {
	window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Esp {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> core::time::Duration {
		uptime()
	}
}

/// Lines sent to the screen together, at most. Each send sets up a window
/// on the display first, which costs about as much as the pixels of a line.
const BATCH_LINES: usize = 8;

/// Hands Slint a line at a time to draw into, and sends them to the screen
/// in batches of lines below one another that cover the same columns.
struct Lines<'a> {
	display: &'a mut Display,
	pixels: [Rgb565Pixel; WIDTH as usize * BATCH_LINES],
	/// The batch waiting to be sent: its first line, how many lines it has,
	/// and the columns they cover.
	batch: Option<(usize, usize, core::ops::Range<usize>)>,
}

impl Lines<'_> {
	/// Sends the lines waiting, if any.
	fn flush(&mut self) {
		let Some((first, count, columns)) = self.batch.take() else {
			return;
		};
		let colors = self.pixels[..columns.len() * count]
			.iter()
			.map(|pixel| Rgb565::from(RawU16::new(pixel.0)));
		let _ = self.display.set_pixels(
			columns.start as u16,
			first as u16,
			columns.end as u16 - 1,
			(first + count - 1) as u16,
			colors,
		);
	}
}

impl LineBufferProvider for &mut Lines<'_> {
	type TargetPixel = Rgb565Pixel;

	fn process_line(
		&mut self,
		line: usize,
		range: core::ops::Range<usize>,
		render_fn: impl FnOnce(&mut [Rgb565Pixel]),
	) {
		let fits = self.batch.as_ref().is_some_and(|(first, count, columns)| {
			*columns == range && first + count == line && *count < BATCH_LINES
		});
		if !fits {
			self.flush();
			self.batch = Some((line, 0, range.clone()));
		}
		let Some((_, count, _)) = &mut self.batch else {
			return;
		};
		let start = *count * range.len();
		render_fn(&mut self.pixels[start..start + range.len()]);
		*count += 1;
	}
}

#[embassy_executor::task]
pub async fn run(board: Board, settings: Settings) {
	let Board {
		mut display,
		mut i2c,
		mut slot,
		..
	} = board;
	let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
	slint::platform::set_platform(Box::new(Esp {
		window: window.clone(),
	}))
	.expect("setting Slint's platform");
	window.set_size(slint::PhysicalSize::new(WIDTH.into(), HEIGHT.into()));
	let free_before = esp_alloc::HEAP.free();
	let mut gui = Gui::new(settings).expect("creating the interface");
	gui.window().show().expect("showing the interface");
	info!(
		"The interface takes {} KB, leaving {} KB free",
		free_before.saturating_sub(esp_alloc::HEAP.free()) / 1024,
		esp_alloc::HEAP.free() / 1024
	);
	let mut lines = Lines {
		display: &mut display,
		pixels: [Rgb565Pixel(0); WIDTH as usize * BATCH_LINES],
		batch: None,
	};
	let mut timeout = ScreenTimeout::new(uptime());
	let mut backlight = u8::MAX;
	let mut touching = None;
	// A serial tap, held until then as a finger would be.
	let mut serial_tap: Option<(Instant, slint::LogicalPosition)> = None;
	loop {
		crate::ui::report_stack();
		slint::platform::update_timers_and_animations();
		let touch = timeout.touch(uptime(), touch_lcd::read_touch(&mut i2c));
		let touch = touch.map(|(x, y)| {
			slint::PhysicalPosition::new(x.into(), y.into()).to_logical(window.scale_factor())
		});
		let event = match (touching, touch) {
			(None, Some(position)) => Some(WindowEvent::PointerPressed {
				position,
				button: PointerEventButton::Left,
			}),
			(Some(previous), Some(position)) if previous != position => {
				Some(WindowEvent::PointerMoved { position })
			}
			(Some(position), None) => Some(WindowEvent::PointerReleased {
				position,
				button: PointerEventButton::Left,
			}),
			_ => None,
		};
		if let Some(event) = event {
			window.dispatch_event(event);
			if touch.is_none() {
				window.dispatch_event(WindowEvent::PointerExited);
			}
		}
		touching = touch;
		while let Ok(message) = TO_UI.try_receive() {
			match message {
				ToUi::Status(status) => gui.set_status(status),
				ToUi::NodeInfo(key, info) => gui.handle_nodeinfo(key, info.as_deref()),
				ToUi::Networks(networks) => gui.set_networks(networks),
				ToUi::Event(_) => {}
			}
		}
		gui.tick();
		while let Some(command) = gui.poll_command() {
			match command {
				// The card is on this side, and quick enough to wait for.
				Command::ExportSettings(text) => {
					let time = gui.local_time();
					gui.settings_exported(slot.export_settings(&text, time));
				}
				Command::ImportSettings => gui.settings_imported(slot.import_settings()),
				command => FROM_UI.send(command).await,
			}
		}
		// Taps over the serial port, in pixels here, for testing.
		while let Ok(input) = INPUT.try_receive() {
			if !timeout.input(uptime()) {
				continue;
			}
			if let Input::Key(key) = input {
				gui.type_key(key);
			}
			if let Input::Press { column, row } = input {
				let position = slint::LogicalPosition::new(column.into(), row.into());
				window.dispatch_event(WindowEvent::PointerPressed {
					position,
					button: PointerEventButton::Left,
				});
				serial_tap = Some((Instant::now() + Duration::from_millis(150), position));
			}
		}
		if let Some((until, position)) = serial_tap
			&& Instant::now() >= until
		{
			window.dispatch_event(WindowEvent::PointerReleased {
				position,
				button: PointerEventButton::Left,
			});
			serial_tap = None;
		}
		let level = timeout.backlight(uptime());
		// What changed while the screen was off is drawn as it wakes.
		if level > 0 {
			let started = Instant::now();
			let drawn = window.draw_if_needed(|renderer| {
				renderer.render_by_line(&mut lines);
				lines.flush();
			});
			if drawn && started.elapsed() > SLOW_DRAW {
				info!("Drawing took {} ms", started.elapsed().as_millis());
			}
		}
		if level != backlight {
			if level == 0 || backlight == 0 {
				touch_lcd::show_picture(lines.display, level > 0);
			}
			touch_lcd::set_backlight(&mut i2c, level);
			backlight = level;
		}
		// While something moves, draw the next frame as soon as possible.
		// The network's thread still goes first.
		if window.has_active_animations() {
			embassy_futures::yield_now().await;
		} else {
			Timer::after(FRAME).await;
		}
	}
}
