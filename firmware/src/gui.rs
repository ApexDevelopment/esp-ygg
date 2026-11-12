//! Runs the graphical interface, made with Slint, on the touch board. Slint
//! draws a line at a time, so the screen needs no frame buffer.

use alloc::boxed::Box;
use alloc::rc::Rc;

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics_core::pixelcolor::Rgb565;
use embedded_graphics_core::pixelcolor::raw::RawU16;
use esp_ygg_gui::MainWindow;
use esp_ygg_gui::slint;
use log::info;
use slint::ComponentHandle;
use slint::platform::software_renderer::{
	LineBufferProvider, MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};

use crate::touch_lcd::{self, Board, Display};
use crate::ui::{INPUT, Input};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
/// How often the touch panel is read, and the screen redrawn if it changed.
const FRAME: Duration = Duration::from_millis(20);
/// A draw slower than this is logged. Redrawing the whole screen takes
/// about 72 ms.
const SLOW_DRAW: Duration = Duration::from_millis(80);

struct Esp {
	window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Esp {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> core::time::Duration {
		core::time::Duration::from_micros(Instant::now().as_micros())
	}
}

/// Hands Slint a line at a time to draw into, and sends each to the screen.
struct Lines<'a> {
	display: &'a mut Display,
	line: [Rgb565Pixel; WIDTH as usize],
}

impl LineBufferProvider for &mut Lines<'_> {
	type TargetPixel = Rgb565Pixel;

	fn process_line(
		&mut self,
		line: usize,
		range: core::ops::Range<usize>,
		render_fn: impl FnOnce(&mut [Rgb565Pixel]),
	) {
		let pixels = &mut self.line[range.clone()];
		render_fn(pixels);
		let colors = pixels
			.iter()
			.map(|pixel| Rgb565::from(RawU16::new(pixel.0)));
		let _ = self.display.set_pixels(
			range.start as u16,
			line as u16,
			range.end as u16 - 1,
			line as u16,
			colors,
		);
	}
}

#[embassy_executor::task]
pub async fn run(board: Board) {
	let Board {
		mut display,
		mut i2c,
		..
	} = board;
	let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
	slint::platform::set_platform(Box::new(Esp {
		window: window.clone(),
	}))
	.expect("setting Slint's platform");
	window.set_size(slint::PhysicalSize::new(WIDTH.into(), HEIGHT.into()));
	let free_before = esp_alloc::HEAP.free();
	let ui = MainWindow::new().expect("creating the interface");
	ui.show().expect("showing the interface");
	info!(
		"The interface takes {} KB, leaving {} KB free",
		free_before.saturating_sub(esp_alloc::HEAP.free()) / 1024,
		esp_alloc::HEAP.free() / 1024
	);
	let mut lines = Lines {
		display: &mut display,
		line: [Rgb565Pixel(0); WIDTH as usize],
	};
	let mut touching = None;
	// A serial tap, held until then as a finger would be.
	let mut serial_tap: Option<(Instant, slint::LogicalPosition)> = None;
	loop {
		slint::platform::update_timers_and_animations();
		let touch = touch_lcd::read_touch(&mut i2c).map(|(x, y)| {
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
		// Taps over the serial port, in pixels here, for testing.
		while let Ok(input) = INPUT.try_receive() {
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
		let started = Instant::now();
		let drawn = window.draw_if_needed(|renderer| {
			renderer.render_by_line(&mut lines);
		});
		if drawn && started.elapsed() > SLOW_DRAW {
			info!("Drawing took {} ms", started.elapsed().as_millis());
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
