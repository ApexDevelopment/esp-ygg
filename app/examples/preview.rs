//! Runs the on-device UI in a terminal, as board 0 of a simulated mesh of
//! three boards in a line. Board 2 echoes console messages back, and in
//! voice builds answers calls.
//!
//! `cargo run -p esp-ygg-app --example preview`, then quit with Ctrl+C.
//! Add `-- touch` for the touch board's larger screen, where clicks are taps.

use std::io;
use std::time::{Duration, Instant as WallClock};

use esp_ygg_app::ui::{self, App, Command, Key, Status, Uplink, WifiNetwork};
use esp_ygg_app::{BoardInfo, Event, Overlay, Settings};
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
	let mut app = App::new(settings, radio.nodes[0].now());

	let touch = std::env::args().any(|argument| argument == "touch");
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
	let result = run(&mut terminal, &mut radio, &mut overlays, &mut app);
	if touch {
		execute!(io::stdout(), DisableMouseCapture)?;
	}
	ratatui::crossterm::terminal::disable_raw_mode()?;
	terminal.clear()?;
	result
}

fn run(
	terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
	radio: &mut Radio,
	overlays: &mut [Overlay],
	app: &mut App,
) -> io::Result<()> {
	let started = WallClock::now();
	let mut last_status = WallClock::now() - Duration::from_secs(1);
	loop {
		radio.set_clock(started.elapsed().as_micros() as u64);
		radio.step();
		for (index, overlay) in overlays.iter_mut().enumerate() {
			let time = overlay.unix_micros(radio.nodes[index].now());
			let board = board_info(index, started, time);
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
				Status::from_node(&radio.nodes[0], &board_info(0, started, time), Uplink::Off);
			app.set_status(status);
			last_status = WallClock::now();
		}
		app.tick(radio.nodes[0].now());
		while let Some(command) = app.poll_command() {
			carry_out(command, radio, &mut overlays[0], app);
		}
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
				app.handle_tap(mouse.column, mouse.row);
			}
			_ => {}
		}
	}
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
