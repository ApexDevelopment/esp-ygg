//! Prints how much memory the interface holds on each screen, counting
//! every allocation, to find what to slim down. Pointers are twice as wide
//! here as on the boards, so the numbers run about twice theirs.
//!
//! `cargo run -p esp-ygg-gui --example heap`

use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use esp_ygg_app::Settings;
use esp_ygg_gui::{Gui, Setting, Setup};
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};

static LIVE: AtomicIsize = AtomicIsize::new(0);

/// The system's allocator, keeping count of the bytes held.
struct Counting;

// SAFETY: passes every call straight to the system's allocator.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(pointer, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Desktop {
	window: Rc<MinimalSoftwareWindow>,
	started: Instant,
}

impl Platform for Desktop {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> Duration {
		self.started.elapsed()
	}
}

/// Draws the screen, as a board would, then prints what is held.
fn report(window: &MinimalSoftwareWindow, screen: &str) {
	let mut line = vec![Rgb565Pixel(0); 320];
	slint::platform::update_timers_and_animations();
	window.request_redraw();
	window.draw_if_needed(|renderer| {
		struct Line<'a>(&'a mut [Rgb565Pixel]);
		impl slint::platform::software_renderer::LineBufferProvider for Line<'_> {
			type TargetPixel = Rgb565Pixel;
			fn process_line(
				&mut self,
				_line: usize,
				range: core::ops::Range<usize>,
				render_fn: impl FnOnce(&mut [Rgb565Pixel]),
			) {
				render_fn(&mut self.0[range]);
			}
		}
		renderer.render_by_line(Line(&mut line));
	});
	drop(line);
	println!(
		"{screen:>10}: {:>4} KB",
		LIVE.load(Ordering::Relaxed) / 1024
	);
}

fn main() {
	let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
	slint::platform::set_platform(Box::new(Desktop {
		window: window.clone(),
		started: Instant::now(),
	}))
	.expect("setting the platform");
	window.set_size(slint::PhysicalSize::new(320, 240));
	let mut gui = Gui::new(Settings::new()).expect("creating the interface");
	gui.window().show().expect("showing the window");
	report(&window, "launcher");
	for (page, name) in [(1, "nodes"), (2, "radio"), (3, "settings")] {
		gui.window().set_page(page);
		report(&window, name);
	}
	let setup = gui.window().global::<Setup>();
	setup.invoke_edit(Setting::Name, "Name".into());
	gui.tick();
	report(&window, "keyboard");
	gui.type_key(esp_ygg_app::ui::Key::Escape);
	gui.tick();
	report(&window, "settings");
}
