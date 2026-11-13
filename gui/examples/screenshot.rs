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

use esp_ygg_app::Event;
use esp_ygg_app::Stream;
use esp_ygg_app::ui::{Neighbor, Status, Uplink};
use esp_ygg_app::{BoardInfo, Settings};
use esp_ygg_gui::{App, Console, Gui, Irc, IrcField, Launcher, Ping, Setup};
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{PrivateKey, PublicKey};

const PAGES: [&str; 4] = ["home", "nodes", "neighbors", "settings"];
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
	let mut settings = Settings::new();
	settings.name = "esp-ygg".into();
	let mut gui = Gui::new(settings).expect("creating the interface");
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
	save_keyboards(&mut gui, &window, &folder);
	save_settings_scrolled(&mut gui, &window, &folder);
	save_ping(&mut gui, &window, &folder, &keys);
	save_console(&mut gui, &window, &folder, &keys);
	save_irc(&mut gui, &window, &folder);
}

/// Saves IRC in both orientations: the form, then a channel after some
/// talk, then the list of conversations.
fn save_irc(gui: &mut Gui, window: &MinimalSoftwareWindow, folder: &str) {
	use esp_ygg_app::ui::Key;
	gui.window().global::<Launcher>().set_open(App::Irc);
	gui.window().global::<Irc>().invoke_edit(IrcField::Server);
	gui.tick();
	for character in "200::5".chars() {
		gui.type_key(Key::Char(character));
	}
	gui.type_key(Key::Enter);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("irc-{orientation}-form"));
	}
	gui.window().global::<Irc>().invoke_connect();
	gui.tick();
	gui.handle_event(Event::StreamOpened(Stream::Irc));
	let lines = [
		":irc.test 001 esp-ygg :Welcome to the mesh",
		":esp-ygg!u@200::1 JOIN #esp-ygg",
		":ada!a@200::2 PRIVMSG #esp-ygg :hello from the desktop",
		":ada!a@200::2 PRIVMSG #esp-ygg :\u{1}ACTION waves\u{1}",
		":bob!b@200::3 PRIVMSG esp-ygg :psst, over here",
	];
	for line in lines {
		gui.handle_event(Event::StreamData {
			stream: Stream::Irc,
			data: format!("{line}\r\n").into_bytes(),
		});
	}
	gui.tick();
	gui.window().global::<Irc>().invoke_write();
	gui.tick();
	for character in "hi ada, it works on the touch board too".chars() {
		gui.type_key(Key::Char(character));
	}
	gui.type_key(Key::Enter);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("irc-{orientation}"));
	}
	gui.window().global::<Irc>().set_choosing(true);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("irc-{orientation}-chats"));
	}
}

/// Saves the launcher with a message waiting, then the Console in both
/// orientations: empty, then after a message and a reply.
fn save_console(gui: &mut Gui, window: &MinimalSoftwareWindow, folder: &str, keys: &[PublicKey]) {
	gui.window().global::<Launcher>().set_open(App::Console);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("console-{orientation}-empty"));
	}
	gui.window().global::<Launcher>().set_open(App::None);
	let from = std::net::Ipv6Addr::from(addr_for_key(&keys[1]).0);
	gui.handle_event(Event::Datagram {
		from: std::net::SocketAddrV6::new(from, 40000, 0, 0),
		data: b"Anyone up for a walk later? The rain has finally stopped.\n".to_vec(),
	});
	save_screen(window, folder, "console-portrait-unread");
	gui.window().global::<Launcher>().set_open(App::Console);
	gui.window().global::<Console>().invoke_write();
	gui.tick();
	for character in "Sounds good".chars() {
		gui.type_key(esp_ygg_app::ui::Key::Char(character));
	}
	gui.type_key(esp_ygg_app::ui::Key::Enter);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("console-{orientation}"));
	}
}

/// Saves the Ping app in both orientations: choosing a node, then after a
/// few round trips to it.
fn save_ping(gui: &mut Gui, window: &MinimalSoftwareWindow, folder: &str, keys: &[PublicKey]) {
	gui.window().set_page(0);
	gui.window().global::<Launcher>().set_open(App::Ping);
	gui.window().global::<Ping>().set_choosing(true);
	gui.tick();
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("ping-{orientation}-choosing"));
	}
	gui.window().global::<Ping>().set_choosing(false);
	let target = std::net::Ipv6Addr::from(addr_for_key(&keys[0]).0);
	gui.window().global::<Ping>().invoke_type_address();
	gui.tick();
	for character in target.to_string().chars() {
		gui.type_key(esp_ygg_app::ui::Key::Char(character));
	}
	gui.type_key(esp_ygg_app::ui::Key::Enter);
	for sequence in 1..=4 {
		let event = if sequence == 3 {
			Event::PingTimeout { target, sequence }
		} else {
			Event::PingReply {
				from: target,
				sequence,
				round_trip: Duration::from_millis(20 + 7 * u64::from(sequence)),
			}
		};
		gui.handle_event(event);
	}
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		save_screen(window, folder, &format!("ping-{orientation}"));
	}
}

/// Saves the keyboard in both orientations: for the name, with letters,
/// after typing on it, and with symbols; then for the channel, as a number
/// pad, and after Enter on a channel that will not do.
fn save_keyboards(gui: &mut Gui, window: &MinimalSoftwareWindow, folder: &str) {
	for (orientation, width, height) in SIZES {
		window.set_size(slint::PhysicalSize::new(width, height));
		gui.window().set_page(3);
		let row_x = if width > height { 200.0 } else { 120.0 };
		let (rows_top, row) = (52.0, (height as f32 - 56.0) / 4.0);
		let key = |column: f32, line: f32| (width as f32 * column, rows_top + row * line);
		tap(gui, window, (row_x, 100.0));
		save_screen(window, folder, &format!("keyboard-{orientation}-letters"));
		// Shift, then F
		tap(gui, window, (12.0, rows_top + row * 2.5));
		tap(gui, window, key(0.42, 1.5));
		save_screen(window, folder, &format!("keyboard-{orientation}-typed"));
		tap(gui, window, (16.0, rows_top + row * 3.5));
		save_screen(window, folder, &format!("keyboard-{orientation}-symbols"));
		// Enter, which applies the name, then the channel, typed as 99
		tap(gui, window, key(0.95, 3.5));
		tap(gui, window, (row_x, 152.0));
		save_screen(window, folder, &format!("keyboard-{orientation}-numbers"));
		tap(gui, window, key(0.3, 2.5));
		tap(gui, window, key(0.3, 2.5));
		tap(gui, window, key(0.9, 3.5));
		save_screen(window, folder, &format!("keyboard-{orientation}-error"));
		// Cancel
		tap(gui, window, (width as f32 - 30.0, 26.0));
	}
}

fn save_screen(window: &MinimalSoftwareWindow, folder: &str, name: &str) {
	let size = window.size();
	let mut pixels = vec![Rgb565Pixel(0); (size.width * size.height) as usize];
	slint::platform::update_timers_and_animations();
	window.request_redraw();
	window.draw_if_needed(|renderer| {
		renderer.render(&mut pixels, size.width as usize);
	});
	let path = Path::new(folder).join(format!("{name}.png"));
	save(&pixels, size.width, size.height, &path);
	println!("{}", path.display());
}

/// Taps the screen, as a finger would, and lets the interface act on it.
fn tap(gui: &mut Gui, window: &MinimalSoftwareWindow, at: (f32, f32)) {
	use slint::platform::{PointerEventButton, WindowEvent};
	let position = slint::LogicalPosition::new(at.0, at.1);
	let button = PointerEventButton::Left;
	window.dispatch_event(WindowEvent::PointerPressed { position, button });
	window.dispatch_event(WindowEvent::PointerReleased { position, button });
	window.dispatch_event(WindowEvent::PointerExited);
	gui.tick();
	for _ in 0..40 {
		std::thread::sleep(Duration::from_millis(10));
		slint::platform::update_timers_and_animations();
	}
}

/// Saves the settings page with the bridge and the hotspot on, after
/// saving without a network or peer, as it looks dragged down a screen at
/// a time, wide.
fn save_settings_scrolled(gui: &mut Gui, window: &MinimalSoftwareWindow, folder: &str) {
	let (_, width, height) = SIZES[0];
	window.set_size(slint::PhysicalSize::new(width, height));
	gui.window().set_page(3);
	let setup = gui.window().global::<Setup>();
	setup.set_bridge(true);
	setup.set_hotspot(true);
	setup.invoke_save();
	gui.tick();
	for screen in 1..=5 {
		drag(window, (200.0, 220.0), (200.0, 40.0));
		let mut pixels = vec![Rgb565Pixel(0); (width * height) as usize];
		window.request_redraw();
		window.draw_if_needed(|renderer| {
			renderer.render(&mut pixels, width as usize);
		});
		let path = Path::new(folder).join(format!("mesh-landscape-settings-{screen}.png"));
		save(&pixels, width, height, &path);
		println!("{}", path.display());
	}
}

/// Drags a finger across the screen in small steps, as a touch panel
/// reports it.
fn drag(window: &MinimalSoftwareWindow, from: (f32, f32), to: (f32, f32)) {
	use slint::platform::{PointerEventButton, WindowEvent};
	const STEPS: usize = 20;
	let at = |step: usize| {
		let progress = step as f32 / STEPS as f32;
		slint::LogicalPosition::new(
			from.0 + (to.0 - from.0) * progress,
			from.1 + (to.1 - from.1) * progress,
		)
	};
	let button = PointerEventButton::Left;
	window.dispatch_event(WindowEvent::PointerPressed {
		position: at(0),
		button,
	});
	for step in 1..=STEPS {
		std::thread::sleep(Duration::from_millis(10));
		slint::platform::update_timers_and_animations();
		window.dispatch_event(WindowEvent::PointerMoved { position: at(step) });
	}
	window.dispatch_event(WindowEvent::PointerReleased {
		position: at(STEPS),
		button,
	});
	// Lets any fling come to rest.
	for _ in 0..100 {
		std::thread::sleep(Duration::from_millis(10));
		slint::platform::update_timers_and_animations();
	}
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
