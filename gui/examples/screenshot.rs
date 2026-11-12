//! Draws the interface with Slint's software renderer, as a board does, and
//! saves it as a PNG.
//!
//! `cargo run -p esp-ygg-gui --example screenshot -- <file.png>`

use std::fs::File;
use std::io::BufWriter;
use std::rc::Rc;
use std::time::Instant;

use esp_ygg_gui::MainWindow;
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

struct Desktop {
	window: Rc<MinimalSoftwareWindow>,
	started: Instant,
}

impl Platform for Desktop {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> core::time::Duration {
		self.started.elapsed()
	}
}

fn main() {
	let path = std::env::args()
		.nth(1)
		.unwrap_or_else(|| "screen.png".into());
	let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
	slint::platform::set_platform(Box::new(Desktop {
		window: window.clone(),
		started: Instant::now(),
	}))
	.expect("setting the platform");
	let ui = MainWindow::new().expect("creating the window");
	ui.show().expect("showing the window");
	window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
	let mut pixels = vec![Rgb565Pixel(0); (WIDTH * HEIGHT) as usize];
	slint::platform::update_timers_and_animations();
	window.draw_if_needed(|renderer| {
		renderer.render(&mut pixels, WIDTH as usize);
	});
	save(&pixels, &path);
	println!("{path}");
}

fn save(pixels: &[Rgb565Pixel], path: &str) {
	let file = BufWriter::new(File::create(path).expect("creating the file"));
	let mut encoder = png::Encoder::new(file, WIDTH, HEIGHT);
	encoder.set_color(png::ColorType::Rgb);
	encoder.set_depth(png::BitDepth::Eight);
	let bytes: Vec<u8> = pixels
		.iter()
		.flat_map(|pixel| {
			let [red, green, blue] = [pixel.0 >> 11, (pixel.0 >> 5) & 0x3f, pixel.0 & 0x1f];
			[(red << 3) as u8, (green << 2) as u8, (blue << 3) as u8]
		})
		.collect();
	encoder
		.write_header()
		.and_then(|mut writer| writer.write_image_data(&bytes))
		.expect("writing the picture");
}
