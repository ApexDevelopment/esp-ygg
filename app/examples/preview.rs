//! Runs the on-device UI on the desktop, as board 0 of a simulated mesh of
//! three boards in a line. Board 2 echoes console messages back, and in
//! voice builds answers calls.
//!
//! `cargo run -p esp-ygg-app --example preview` shows it in a terminal, the
//! size of the Cardputer's screen, until Ctrl+C. Add `-- touch` for the touch
//! board's larger screen, where mouse clicks are taps.
//!
//! `-- png <folder> <command>...` instead draws the touch board's screen
//! pixel for pixel, as the board does, and runs the commands `drive.py`
//! takes: `type`, `key`, `tap` and `wait`. Each `screen` saves a picture of
//! the screen in the folder.

use std::cell::RefCell;
use std::convert::Infallible;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant as WallClock};

use esp_ygg_app::ui::remote::{self, Remote};
use esp_ygg_app::ui::{self, App, Command, Key, Status, Uplink, WifiNetwork};
use esp_ygg_app::{BoardInfo, Event, Overlay, Settings};
use mousefood::embedded_graphics::Pixel;
use mousefood::embedded_graphics::draw_target::DrawTarget;
use mousefood::embedded_graphics::geometry::{OriginDimensions, Size};
use mousefood::embedded_graphics::pixelcolor::{Rgb565, Rgb888, RgbColor};
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig, TerminalAlignment};
use ratatui::crossterm::event::{
	self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEventKind,
	KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use yggdrasil_espnow::Config;
use yggdrasil_espnow::sim::{Radio, private_key};
use yggdrasil_espnow::yggdrasil_proto::requests::build_nodeinfo;

const BOARDS: [&str; 3] = ["preview", "relay", "echo"];

fn main() -> io::Result<()> {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	match arguments.split_first() {
		Some((mode, rest)) if mode == "png" => {
			let Some((folder, commands)) = rest.split_first() else {
				return Err(io::Error::other("Usage: png <folder> <command>..."));
			};
			screenshots(Path::new(folder), commands)
		}
		Some((mode, _)) => in_terminal(mode == "touch"),
		None => in_terminal(false),
	}
}

/// The simulated mesh, and the UI of its first board.
struct Sim {
	radio: Radio,
	overlays: Vec<Overlay>,
	app: App,
	started: WallClock,
	last_status: WallClock,
}

impl Sim {
	fn new() -> Self {
		let mut radio = Radio::new();
		for (index, name) in BOARDS.iter().enumerate() {
			let mut config = Config::new(private_key(index as u8 + 1));
			let given = serde_json::json!({ "name": name });
			config.nodeinfo = build_nodeinfo(given.as_object().unwrap(), None).unwrap();
			radio.add_node(config);
		}
		radio.connect(0, 1);
		radio.connect(1, 2);
		let mut overlays: Vec<Overlay> = (0..BOARDS.len())
			.map(|index| Overlay::new(&radio.nodes[index], index as u64 + 1, true))
			.collect();
		// The first board knows the time, as a bridged board would from NTP.
		let since_epoch = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default();
		overlays[0].set_time(since_epoch.as_micros() as u64, 2, radio.nodes[0].now());
		#[cfg(feature = "voice")]
		for (index, overlay) in overlays.iter_mut().enumerate() {
			overlay.start_phone(BOARDS[index], index as u16);
		}
		let mut settings = Settings::new();
		settings.name = BOARDS[0].into();
		let app = App::new(settings, radio.nodes[0].now());
		Sim {
			radio,
			overlays,
			app,
			started: WallClock::now(),
			last_status: WallClock::now() - Duration::from_secs(1),
		}
	}

	/// Runs the mesh, and the UI's timers and commands, once.
	fn step(&mut self) {
		let Sim {
			radio,
			overlays,
			app,
			started,
			last_status,
		} = self;
		radio.set_clock(started.elapsed().as_micros() as u64);
		radio.step();
		for (index, overlay) in overlays.iter_mut().enumerate() {
			let time = overlay.unix_micros(radio.nodes[index].now());
			let board = board_info(index, *started, time);
			overlay.poll(&mut radio.nodes[index], &board);
		}
		let now = radio.nodes[2].now();
		echo_datagrams(&mut overlays[2], now);
		while let Some(event) = overlays[0].poll_event() {
			app.handle_event(event);
		}
		while let Some((key, info)) = radio.nodes[0].poll_nodeinfo() {
			app.handle_nodeinfo(key, info);
		}
		if last_status.elapsed() >= Duration::from_millis(500) {
			let time = overlays[0].unix_micros(radio.nodes[0].now());
			let status =
				Status::from_node(&radio.nodes[0], &board_info(0, *started, time), Uplink::Off);
			app.set_status(status);
			*last_status = WallClock::now();
		}
		app.tick(radio.nodes[0].now());
		while let Some(command) = app.poll_command() {
			carry_out(command, radio, &mut overlays[0], app);
		}
	}
}

fn in_terminal(touch: bool) -> io::Result<()> {
	let mut sim = Sim::new();
	let rows = if touch { 18 } else { 13 };
	ratatui::crossterm::terminal::enable_raw_mode()?;
	if touch {
		execute!(io::stdout(), EnableMouseCapture)?;
	}
	let options = TerminalOptions {
		viewport: Viewport::Fixed(Rect::new(0, 0, 40, rows)),
	};
	let mut terminal = Terminal::with_options(
		ratatui::backend::CrosstermBackend::new(io::stdout()),
		options,
	)?;
	terminal.clear()?;
	let result = run_in_terminal(&mut terminal, &mut sim);
	if touch {
		execute!(io::stdout(), DisableMouseCapture)?;
	}
	ratatui::crossterm::terminal::disable_raw_mode()?;
	terminal.clear()?;
	result
}

fn run_in_terminal(
	terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
	sim: &mut Sim,
) -> io::Result<()> {
	loop {
		sim.step();
		let app = &mut sim.app;
		terminal.draw(|frame| ui::draw(frame, app))?;
		if !event::poll(Duration::from_millis(5))? {
			continue;
		}
		match event::read()? {
			TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
				if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
					return Ok(());
				}
				if let Some(key) = map_key(key.code) {
					app.handle_key(key);
				}
			}
			TermEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
				app.handle_press(mouse.column, mouse.row);
			}
			TermEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Up(MouseButton::Left) => {
				app.handle_tap(mouse.column, mouse.row);
				app.handle_release();
			}
			_ => {}
		}
	}
}

/// The touch board's screen, turned on its side.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
/// How long the mesh runs after each command, so the screen settles.
const SETTLE: Duration = Duration::from_millis(200);

/// The touch board's screen in memory. Clones share the same pixels, so the
/// terminal can draw into one while another saves them.
#[derive(Clone)]
struct Pixels(Rc<RefCell<Vec<Rgb565>>>);

impl OriginDimensions for Pixels {
	fn size(&self) -> Size {
		Size::new(WIDTH, HEIGHT)
	}
}

impl DrawTarget for Pixels {
	type Color = Rgb565;
	type Error = Infallible;

	fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
	where
		I: IntoIterator<Item = Pixel<Self::Color>>,
	{
		let mut screen = self.0.borrow_mut();
		for Pixel(point, color) in pixels {
			if let (Ok(x @ 0..WIDTH), Ok(y @ 0..HEIGHT)) =
				(u32::try_from(point.x), u32::try_from(point.y))
			{
				screen[(y * WIDTH + x) as usize] = color;
			}
		}
		Ok(())
	}
}

impl Pixels {
	fn save(&self, path: &Path) -> io::Result<()> {
		let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), WIDTH, HEIGHT);
		encoder.set_color(png::ColorType::Rgb);
		encoder.set_depth(png::BitDepth::Eight);
		let bytes: Vec<u8> = self
			.0
			.borrow()
			.iter()
			.flat_map(|&color| {
				let color = Rgb888::from(color);
				[color.r(), color.g(), color.b()]
			})
			.collect();
		encoder
			.write_header()
			.and_then(|mut writer| writer.write_image_data(&bytes))
			.map_err(io::Error::other)
	}
}

/// Runs `commands` on the touch board's screen, drawn as the board draws it,
/// and saves a picture at each `screen`.
fn screenshots(folder: &Path, commands: &[String]) -> io::Result<()> {
	std::fs::create_dir_all(folder)?;
	let pixels = Pixels(Rc::new(RefCell::new(vec![
		Rgb565::BLACK;
		(WIDTH * HEIGHT) as usize
	])));
	// As the touch board sets up its screen, in firmware/src/touch_lcd.rs
	let config = EmbeddedBackendConfig {
		font_regular: mousefood::fonts::mono_8x13_atlas(),
		vertical_alignment: TerminalAlignment::End,
		horizontal_alignment: TerminalAlignment::Start,
		..Default::default()
	};
	let display = Box::leak(Box::new(pixels.clone()));
	let mut terminal =
		Terminal::new(EmbeddedBackend::new(display, config)).map_err(drawing_failed)?;
	let mut sim = Sim::new();
	let mut saved = 0;
	run_for(&mut sim, &mut terminal, SETTLE)?;
	for command in commands {
		if let Some(seconds) = command.strip_prefix("wait ") {
			let seconds: f64 = seconds
				.parse()
				.map_err(|_| io::Error::other(format!("Not a number of seconds: {seconds}")))?;
			run_for(&mut sim, &mut terminal, Duration::from_secs_f64(seconds))?;
			continue;
		}
		match remote::parse(command).map_err(io::Error::other)? {
			Remote::Keys(keys) => keys.into_iter().for_each(|key| sim.app.handle_key(key)),
			Remote::Tap(column, row) => {
				sim.app.handle_press(column, row);
				sim.app.handle_tap(column, row);
				sim.app.handle_release();
			}
			Remote::Screen => {
				saved += 1;
				let path: PathBuf = folder.join(format!("screen-{saved:02}.png"));
				pixels.save(&path)?;
				println!("{}", path.display());
			}
			Remote::Talk(_) | Remote::Mute(_) => {
				return Err(io::Error::other("The preview has no audio"));
			}
			Remote::Drag(..) => {
				return Err(io::Error::other("Drags are for the graphical interface"));
			}
			Remote::Lock => {
				let locked = sim.app.is_locked();
				sim.app.set_locked(!locked);
			}
		}
		run_for(&mut sim, &mut terminal, SETTLE)?;
	}
	Ok(())
}

type PictureTerminal = Terminal<EmbeddedBackend<'static, Pixels, Rgb565>>;

fn drawing_failed(error: mousefood::error::Error) -> io::Error {
	io::Error::other(format!("Drawing failed: {error:?}"))
}

/// Runs the mesh for `duration`, drawing after each step as the board does.
fn run_for(sim: &mut Sim, terminal: &mut PictureTerminal, duration: Duration) -> io::Result<()> {
	let until = WallClock::now() + duration;
	while WallClock::now() < until {
		sim.step();
		terminal
			.draw(|frame| ui::draw(frame, &sim.app))
			.map_err(drawing_failed)?;
		std::thread::sleep(Duration::from_millis(5));
	}
	Ok(())
}

fn board_info(index: usize, started: WallClock, time: Option<u64>) -> BoardInfo {
	BoardInfo {
		name: BOARDS[index].into(),
		model: "Simulated board".into(),
		uptime: started.elapsed(),
		free_heap: None,
		time,
	}
}

const NO_SD_CARD: &str = "The preview has no SD card";

fn carry_out(command: Command, radio: &mut Radio, overlay: &mut Overlay, app: &mut App) {
	match command {
		Command::Ping(target) => {
			let _ = overlay.ping(target, radio.nodes[0].now());
		}
		Command::SendDatagram(to, data) => {
			let _ = overlay.send_datagram(to, &data);
		}
		Command::RequestNodeInfo(key) => radio.nodes[0].request_nodeinfo(key),
		Command::Fetch(to, request) => {
			let _ = overlay.fetch(to, request, radio.nodes[0].now());
		}
		Command::OpenStream(stream, to) => {
			let _ = overlay.open_stream(stream, to);
		}
		Command::StreamSend(stream, data) => {
			let _ = overlay.stream_send(stream, &data);
		}
		Command::CloseStream(stream) => overlay.close_stream(stream),
		Command::ScanWifi => app.set_networks(vec![
			WifiNetwork {
				ssid: "PHOTON".into(),
				signal: -52,
				secured: true,
			},
			WifiNetwork {
				ssid: "Coffee shop".into(),
				signal: -71,
				secured: false,
			},
		]),
		// A board would store the settings and restart.
		Command::Save(_) => {}
		Command::ExportSettings(_) => app.settings_exported(Err(NO_SD_CARD.into())),
		Command::ImportSettings => app.settings_imported(Err(NO_SD_CARD.into())),
		#[cfg(feature = "voice")]
		Command::Call(to) => {
			let _ = overlay.call(to, radio.nodes[0].now());
		}
		#[cfg(feature = "voice")]
		Command::Answer => {
			let _ = overlay.answer(radio.nodes[0].now());
		}
		#[cfg(feature = "voice")]
		Command::HangUp => {
			let _ = overlay.hang_up(radio.nodes[0].now());
		}
		// The desktop preview has no speaker.
		#[cfg(feature = "voice")]
		Command::PreviewRinger { .. } => {}
	}
}

fn echo_datagrams(overlay: &mut Overlay, now: yggdrasil_espnow::Instant) {
	while let Some(event) = overlay.poll_event() {
		match event {
			Event::Datagram { from, data } => {
				let mut reply = b"echo: ".to_vec();
				reply.extend_from_slice(&data);
				let _ = overlay.send_datagram(from, &reply);
			}
			#[cfg(feature = "voice")]
			Event::Call(esp_ygg_app::voice::iax2::CallEvent::Incoming { .. }) => {
				let _ = overlay.answer(now);
			}
			_ => {}
		}
	}
	let _ = now;
}

fn map_key(code: KeyCode) -> Option<Key> {
	Some(match code {
		KeyCode::Char(character) => Key::Char(character),
		KeyCode::Enter => Key::Enter,
		KeyCode::Backspace => Key::Backspace,
		KeyCode::Tab => Key::Tab,
		KeyCode::Esc => Key::Escape,
		KeyCode::Up => Key::Up,
		KeyCode::Down => Key::Down,
		KeyCode::Left => Key::Left,
		KeyCode::Right => Key::Right,
		_ => return None,
	})
}
