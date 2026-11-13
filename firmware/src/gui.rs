//! Runs the graphical interface, made with Slint, on the touch board. Slint
//! draws a line at a time, so the screen needs no frame buffer.

use alloc::boxed::Box;
use alloc::rc::Rc;

use embassy_time::{Duration, Instant, Timer};
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

use crate::touch_lcd::{self, Board, Panel};
use crate::ui::{FROM_UI, INPUT, Input, TO_UI, ToUi, uptime};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
/// How often the touch panel is read, and the screen redrawn if it changed.
const FRAME: Duration = Duration::from_millis(20);
/// A draw slower than this is logged. Redrawing the whole screen takes
/// about 60 ms, switching pages about 40, and scrolling about 30 a frame.
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

/// Hands Slint a line at a time to draw into, and sends them to the screen
/// in batches of lines below one another that cover the same columns. Each
/// batch goes out while Slint draws the next.
struct Lines<'a> {
	panel: &'a mut Panel,
	/// The batch being drawn: its first line, how many lines it has, and the
	/// columns they cover.
	batch: Option<(usize, usize, core::ops::Range<usize>)>,
}

impl Lines<'_> {
	/// Sends the lines drawn, if any.
	fn flush(&mut self) {
		let Some((first, count, columns)) = self.batch.take() else {
			return;
		};
		self.panel.send(
			(columns.start as u16, columns.end as u16 - 1),
			(first as u16, (first + count - 1) as u16),
			columns.len() * count,
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
			*columns == range
				&& first + count == line
				&& (count + 1) * range.len() <= touch_lcd::BATCH_PIXELS
		});
		if !fits {
			self.flush();
			self.batch = Some((line, 0, range.clone()));
		}
		let Some((_, count, _)) = &mut self.batch else {
			return;
		};
		let start = *count * range.len();
		let pixels = &mut self.panel.pixels()[start..start + range.len()];
		// SAFETY: Slint's pixels are plain RGB565 values.
		let pixels = unsafe { &mut *(core::ptr::from_mut(pixels) as *mut [Rgb565Pixel]) };
		render_fn(pixels);
		*count += 1;
	}
}

/// How long a serial tap holds the screen, as a finger would.
const TAP_HOLD: Duration = Duration::from_millis(150);
/// How long a serial drag takes from end to end.
const DRAG: Duration = Duration::from_millis(300);

/// A finger moved over the screen by commands from the serial port, for
/// testing.
#[derive(Default)]
struct SerialFinger {
	stroke: Option<Stroke>,
}

struct Stroke {
	from: slint::LogicalPosition,
	to: slint::LogicalPosition,
	started: Instant,
	length: Duration,
}

impl SerialFinger {
	fn start(
		&mut self,
		window: &MinimalSoftwareWindow,
		from: (u16, u16),
		to: (u16, u16),
		length: Duration,
	) {
		let point = |(x, y): (u16, u16)| slint::LogicalPosition::new(x.into(), y.into());
		self.stroke = Some(Stroke {
			from: point(from),
			to: point(to),
			started: Instant::now(),
			length,
		});
		window.dispatch_event(WindowEvent::PointerPressed {
			position: point(from),
			button: PointerEventButton::Left,
		});
	}

	/// Moves the finger along, and lifts it at the end.
	fn update(&mut self, window: &MinimalSoftwareWindow) {
		let Some(stroke) = &self.stroke else {
			return;
		};
		let done = (stroke.started.elapsed().as_micros() as f32 / stroke.length.as_micros() as f32)
			.min(1.0);
		let position = slint::LogicalPosition::new(
			stroke.from.x + (stroke.to.x - stroke.from.x) * done,
			stroke.from.y + (stroke.to.y - stroke.from.y) * done,
		);
		window.dispatch_event(WindowEvent::PointerMoved { position });
		if done >= 1.0 {
			window.dispatch_event(WindowEvent::PointerReleased {
				position,
				button: PointerEventButton::Left,
			});
			window.dispatch_event(WindowEvent::PointerExited);
			self.stroke = None;
		}
	}
}

#[embassy_executor::task]
pub async fn run(board: Board, settings: Settings) {
	let Board {
		display,
		mut i2c,
		mut slot,
		..
	} = board;
	let mut panel = Panel::new(display);
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
	let mut timeout = ScreenTimeout::new(uptime());
	let mut backlight = u8::MAX;
	let mut touching = None;
	let mut serial_finger = SerialFinger::default();
	loop {
		let frame_started = Instant::now();
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
				ToUi::Event(event) => gui.handle_event(event),
			}
		}
		gui.tick();
		while let Some(command) = gui.poll_command() {
			match command {
				// The card is on this side, and quick enough to wait for.
				Command::ExportSettings(text) => {
					let time = gui.local_time();
					panel.finish();
					gui.settings_exported(slot.export_settings(&text, time));
				}
				Command::ImportSettings => {
					panel.finish();
					gui.settings_imported(slot.import_settings());
				}
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
			match input {
				Input::Press { column, row } => {
					serial_finger.start(&window, (column, row), (column, row), TAP_HOLD)
				}
				Input::Drag { from, to } => serial_finger.start(&window, from, to, DRAG),
				_ => {}
			}
		}
		serial_finger.update(&window);
		let level = timeout.backlight(uptime());
		// What changed while the screen was off is drawn as it wakes.
		if level > 0 {
			let started = Instant::now();
			let mut lines = Lines {
				panel: &mut panel,
				batch: None,
			};
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
				panel.show_picture(level > 0);
			}
			touch_lcd::set_backlight(&mut i2c, level);
			backlight = level;
		}
		// While something moves, draw the next frame as soon as possible.
		// Otherwise check for touches each frame, however long drawing the
		// last one took. The network's thread goes first either way.
		let next_frame = frame_started + FRAME;
		if window.has_active_animations() || Instant::now() >= next_frame {
			embassy_futures::yield_now().await;
		} else {
			Timer::at(next_frame).await;
		}
	}
}
