//! Draws the interface with Slint's software renderer, as a board does, and
//! saves each page in both orientations as PNGs: first as a board shows
//! them just after starting, with nothing to list, then for a small made-up
//! mesh.
//!
//! `cargo run -p esp-ygg-gui --example screenshot -- <folder>`

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use esp_ygg_app::BoardInfo;
use esp_ygg_app::ui::{Neighbor, Status, Uplink};
use esp_ygg_gui::Gui;
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{PrivateKey, PublicKey};

const PAGES: [&str; 3] = ["home", "nodes", "neighbors"];
const SIZES: [(&str, u32, u32); 2] = [("landscape", 320, 240), ("portrait", 240, 320)];

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

fn main() {
	let folder = std::env::args().nth(1).unwrap_or_else(|| "screens".into());
	std::fs::create_dir_all(&folder).expect("creating the folder");
	let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
	slint::platform::set_platform(Box::new(Desktop {
		window: window.clone(),
		started: Instant::now(),
	}))
	.expect("setting the platform");
	let mut gui = Gui::new().expect("creating the interface");
	gui.window().show().expect("showing the window");
	save_pages(&gui, &window, &folder, "starting");
	let keys: Vec<PublicKey> = (2..6)
		.map(|seed| PrivateKey::from_seed(&[seed; 32]).public_key())
		.collect();
	gui.set_status(status(&keys));
	// Names come back for some nodes and not others.
	gui.window().set_page(1);
	gui.tick();
	for (key, name) in keys.iter().zip(["kitchen", "attic"]) {
		let info = format!(r#"{{"name":"{name}"}}"#);
		gui.handle_nodeinfo(*key, Some(info.as_bytes()));
	}
	save_pages(&gui, &window, &folder, "mesh");
}

/// Saves every page in both orientations, named after `state`.
fn save_pages(gui: &Gui, window: &MinimalSoftwareWindow, folder: &str, state: &str) {
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		for (index, page) in PAGES.iter().enumerate() {
			gui.window().set_page(index as i32);
			let mut pixels = vec![Rgb565Pixel(0); (width * height) as usize];
			slint::platform::update_timers_and_animations();
			window.request_redraw();
			window.draw_if_needed(|renderer| {
				renderer.render(&mut pixels, width as usize);
			});
			let path = Path::new(folder).join(format!("{state}-{orientation}-{page}.png"));
			save(&pixels, width, height, &path);
			println!("{}", path.display());
		}
	}
}

/// A board a few minutes after starting, with two boards in radio range,
/// one of them still linking, and two more nodes beyond them.
fn status(keys: &[PublicKey]) -> Status {
	let own = PrivateKey::from_seed(&[1; 32]).public_key();
	Status {
		address: std::net::Ipv6Addr::from(addr_for_key(&own).0),
		public_key: own,
		neighbors: vec![
			Neighbor {
				mac: [0x9c, 0xcc, 0x01, 0xd9, 0x1c, 0x40],
				key: Some(keys[0]),
				signal: Some(-58),
				loss: Some(2),
			},
			Neighbor {
				mac: [0xe8, 0xf6, 0x0a, 0xf6, 0x77, 0x08],
				key: None,
				signal: Some(-81),
				loss: None,
			},
		],
		peers: vec![keys[0]],
		known: keys.to_vec(),
		uplink: Uplink::Off,
		hotspot: None,
		board: BoardInfo {
			name: "esp-ygg".into(),
			model: "Waveshare ESP32-C6-Touch-LCD-2.8".into(),
			uptime: Duration::from_secs(5 * 60),
			free_heap: Some(159 * 1024),
			time: None,
		},
	}
}

fn save(pixels: &[Rgb565Pixel], width: u32, height: u32, path: &Path) {
	let file = BufWriter::new(File::create(path).expect("creating the file"));
	let mut encoder = png::Encoder::new(file, width, height);
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
