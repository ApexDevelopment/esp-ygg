//! WiFi warnings stay visible through retries and clear on recovery.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use esp_ygg_app::ui::{Status, Uplink};
use esp_ygg_app::{BoardInfo, Settings};
use esp_ygg_gui::{Gui, Notice};
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};
use yggdrasil_espnow::PrivateKey;

struct Headless {
	window: Rc<MinimalSoftwareWindow>,
	clock: Rc<Cell<Duration>>,
}

impl Platform for Headless {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> Duration {
		self.clock.get()
	}
}

fn status(uplink: Uplink) -> Status {
	Status {
		address: "201::1".parse().unwrap(),
		public_key: PrivateKey::from_seed(&[1; 32]).public_key(),
		neighbors: Vec::new(),
		peers: Vec::new(),
		known: Vec::new(),
		uplink,
		hotspot: None,
		board: BoardInfo::default(),
	}
}

fn render(window: &MinimalSoftwareWindow) -> Vec<u16> {
	let size = window.size();
	let mut pixels = vec![Rgb565Pixel(0); (size.width * size.height) as usize];
	slint::platform::update_timers_and_animations();
	window.request_redraw();
	window.draw_if_needed(|renderer| {
		renderer.render(&mut pixels, size.width as usize);
	});
	pixels.into_iter().map(|pixel| pixel.0).collect()
}

#[test]
fn warning_survives_notice_timeout_and_clears_after_recovery() {
	let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
	let clock = Rc::new(Cell::new(Duration::ZERO));
	slint::platform::set_platform(Box::new(Headless {
		window: window.clone(),
		clock: clock.clone(),
	}))
	.unwrap();
	let mut gui = Gui::new(Settings::new()).unwrap();
	gui.window().show().unwrap();
	for (width, height) in [(320, 240), (240, 320)] {
		window.set_size(slint::PhysicalSize::new(width, height));
		gui.set_status(status(Uplink::JoiningWifi));
		let joining = render(&window);
		gui.set_status(status(Uplink::WifiFailed));
		let failed = render(&window);
		assert!(joining != failed, "failure must appear on screen");

		clock.set(clock.get() + Duration::from_secs(5));
		assert!(
			render(&window) == failed,
			"failure must outlast transient notices"
		);
		gui.window()
			.global::<Notice>()
			.set_text("Settings saved".into());
		assert!(render(&window) != failed);
		clock.set(clock.get() + Duration::from_secs(5));
		assert!(
			render(&window) == failed,
			"warning must return after another notice"
		);

		gui.set_status(status(Uplink::Connecting));
		assert!(
			render(&window) == joining,
			"joining WiFi must clear the warning"
		);
		gui.set_status(status(Uplink::WifiFailed));
		render(&window);
		gui.set_status(status(Uplink::Off));
		assert!(
			render(&window) == joining,
			"disabling the bridge must clear the warning"
		);
	}
}
