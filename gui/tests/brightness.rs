//! The brightness slider changes the screen's brightness as it moves, and
//! keeps the change once saved.

use std::rc::Rc;
use std::time::{Duration, Instant};

use esp_ygg_app::Settings;
use esp_ygg_gui::{Gui, Setup};
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};

struct Headless {
	window: Rc<MinimalSoftwareWindow>,
	started: Instant,
}

impl Platform for Headless {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> Duration {
		self.started.elapsed()
	}
}

/// Where the slider's row is, on a screen tall enough to show it unscrolled.
const ROW: f32 = 430.0;

/// Moves a finger along the slider's row, as a touch panel reports it.
fn drag(window: &MinimalSoftwareWindow, gui: &mut Gui, from: f32, to: f32) {
	let at = |x| slint::LogicalPosition::new(x, ROW);
	let button = PointerEventButton::Left;
	window.dispatch_event(WindowEvent::PointerPressed {
		position: at(from),
		button,
	});
	for step in 1..=10 {
		std::thread::sleep(Duration::from_millis(20));
		slint::platform::update_timers_and_animations();
		let x = from + (to - from) * step as f32 / 10.0;
		window.dispatch_event(WindowEvent::PointerMoved { position: at(x) });
	}
	window.dispatch_event(WindowEvent::PointerReleased {
		position: at(to),
		button,
	});
	window.dispatch_event(WindowEvent::PointerExited);
	gui.tick();
}

#[test]
fn brightness_follows_the_slider_until_left_unsaved() {
	let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
	slint::platform::set_platform(Box::new(Headless {
		window: window.clone(),
		started: Instant::now(),
	}))
	.unwrap();
	let mut gui = Gui::new(Settings::new()).unwrap();
	gui.window().show().unwrap();
	window.set_size(slint::PhysicalSize::new(320, 600));
	gui.window().set_page(3);
	gui.tick();
	assert_eq!(gui.brightness(), 100);

	drag(&window, &mut gui, 290.0, 20.0);
	assert_eq!(gui.brightness(), 10);
	drag(&window, &mut gui, 110.0, 200.0);
	let picked = gui.brightness();
	assert!((40..=70).contains(&picked), "{picked}");

	// Leaving unsaved shows the saved brightness again.
	gui.window().set_page(0);
	assert_eq!(gui.brightness(), 100);

	gui.window().set_page(3);
	gui.window().global::<Setup>().invoke_save();
	gui.tick();
	gui.window().set_page(0);
	assert_eq!(gui.brightness(), picked);
}
