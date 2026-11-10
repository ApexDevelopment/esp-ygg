extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Instant, PrivateKey, PublicKey};

use super::*;

fn key(seed: u8) -> PublicKey {
	PrivateKey::from_seed(&[seed; 32]).public_key()
}

fn address(key: &PublicKey) -> Ipv6Addr {
	Ipv6Addr::from(addr_for_key(key).0)
}

fn at(seconds: u64) -> Instant {
	Instant::from_duration(Duration::from_secs(seconds))
}

fn status(peers: &[PublicKey], known: &[PublicKey]) -> Status {
	let own = key(1);
	Status {
		address: address(&own),
		public_key: own,
		neighbors: peers
			.iter()
			.enumerate()
			.map(|(index, key)| Neighbor {
				mac: [2, 0, 0, 0, 0, index as u8],
				key: Some(*key),
				signal: None,
				loss: None,
			})
			.collect(),
		peers: peers.to_vec(),
		known: known.to_vec(),
		uplink: Uplink::Off,
		hotspot: None,
		board: BoardInfo::default(),
	}
}

fn commands(app: &mut App) -> Vec<Command> {
	core::iter::from_fn(|| app.poll_command()).collect()
}

fn press(app: &mut App, keys: &[Key]) {
	for &key in keys {
		app.handle_key(key);
	}
}

/// Opens a screen from the home menu, or the apps menu, by its name.
fn open(app: &mut App, name: &str) {
	press(app, &[Key::Escape, Key::Escape, Key::Escape]);
	if APPS.iter().any(|(label, _)| *label == name) {
		choose(app, MENU, "Apps");
		choose(app, APPS, name);
	} else {
		choose(app, MENU, name);
	}
}

fn choose(app: &mut App, items: &[(&str, Screen)], name: &str) {
	let index = items
		.iter()
		.position(|(label, _)| *label == name)
		.expect("no such menu item");
	for _ in 0..items.len() {
		app.handle_key(Key::Up);
	}
	for _ in 0..index {
		app.handle_key(Key::Down);
	}
	app.handle_key(Key::Enter);
}

fn type_text(app: &mut App, text: &str) {
	for character in text.chars() {
		app.handle_key(Key::Char(character));
	}
}

/// Renders the app and returns the screen as lines of text.
fn screen(app: &App) -> Vec<String> {
	let mut terminal = Terminal::new(TestBackend::new(40, 13)).unwrap();
	terminal.draw(|frame| draw(frame, app)).unwrap();
	super::remote::screen_text(terminal.backend().buffer())
}

#[test]
fn pings_a_node_picked_from_the_network() {
	let mut app = App::new(Settings::new(), at(0));
	let peer = key(2);
	app.set_status(status(&[peer], &[peer]));
	press(&mut app, &[Key::Enter]);
	app.tick(at(1));
	assert_eq!(commands(&mut app), [Command::RequestNodeInfo(peer)]);
	press(&mut app, &[Key::Enter, Key::Char('p')]);
	app.tick(at(2));
	app.tick(at(2));
	app.tick(at(3));
	let target = address(&peer);
	assert_eq!(
		commands(&mut app),
		[Command::Ping(target), Command::Ping(target)]
	);

	app.handle_event(Event::PingReply {
		from: target,
		sequence: 0,
		round_trip: Duration::from_millis(42),
	});
	let shown = screen(&app).join("\n");
	assert!(shown.contains("#0  42 ms"), "{shown}");
	assert!(shown.contains("avg 42 ms"), "{shown}");
}

#[test]
fn names_nodes_from_their_nodeinfo_and_retries_lookups() {
	let mut app = App::new(Settings::new(), at(0));
	let (named, silent) = (key(2), key(3));
	app.set_status(status(&[], &[named, silent]));
	press(&mut app, &[Key::Enter]);
	app.tick(at(1));
	assert_eq!(commands(&mut app).len(), 2);

	app.handle_nodeinfo(
		named,
		Some(br#"{"name":"kitchen","buildname":"esp-ygg"}"#.to_vec()),
	);
	app.handle_nodeinfo(silent, None);
	assert_eq!(commands(&mut app), [Command::RequestNodeInfo(silent)]);
	app.handle_nodeinfo(silent, None);
	app.handle_nodeinfo(silent, None);
	// Out of attempts, so no more requests.
	assert_eq!(commands(&mut app), [Command::RequestNodeInfo(silent)]);

	let shown = screen(&app).join("\n");
	assert!(shown.contains("kitchen"), "{shown}");
	assert!(shown.contains(&address(&silent).to_string()), "{shown}");
}

#[test]
fn sends_console_messages_to_the_console_port() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "UDP Console");
	type_text(&mut app, "200::1");
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "hello");
	press(&mut app, &[Key::Enter]);
	let destination = SocketAddrV6::new("200::1".parse().unwrap(), crate::CONSOLE_PORT, 0, 0);
	assert_eq!(
		commands(&mut app),
		[Command::SendDatagram(destination, b"hello\n".to_vec())]
	);

	app.handle_event(Event::Datagram {
		from: SocketAddrV6::new("200::2".parse().unwrap(), 5555, 0, 0),
		data: b"hi back\n".to_vec(),
	});
	let shown = screen(&app).join("\n");
	assert!(shown.contains("> hello"), "{shown}");
	assert!(shown.contains("200::2 hi back"), "{shown}");
}

#[test]
fn counts_unread_messages_on_the_home_screen() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	app.handle_event(Event::Datagram {
		from: SocketAddrV6::new("200::2".parse().unwrap(), 4242, 0, 0),
		data: b"ping?".to_vec(),
	});
	assert!(screen(&app).join("\n").contains("Apps (1 new)"));
	press(&mut app, &[Key::Down, Key::Down, Key::Down, Key::Enter]);
	assert!(screen(&app).join("\n").contains("UDP Console (1 new)"));
	open(&mut app, "UDP Console");
	press(&mut app, &[Key::Escape]);
	let shown = screen(&app).join("\n");
	assert!(
		shown.contains("Gopher") && !shown.contains("new"),
		"{shown}"
	);
}

#[test]
fn edits_and_saves_settings() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "Settings");
	// Name
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "kitchen");
	press(&mut app, &[Key::Enter]);
	// A time zone, which replaces the old value, "+00:00", as it is typed
	press(&mut app, &[Key::Down, Key::Down, Key::Enter]);
	type_text(&mut app, "-5");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("UTC-05:00"));
	press(&mut app, &[Key::Down, Key::Enter]);
	assert!(screen(&app).join("\n").contains("Status pages off"));
	// The bridge's own menu, marked with an arrow
	press(&mut app, &[Key::Down]);
	assert!(screen(&app).join("\n").contains("Bridge       off"));
	assert!(screen(&app).join("\n").contains('›'));
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app)[0].contains("Bridge"));
	// Bridge on, then pick a network from a scan
	press(&mut app, &[Key::Enter, Key::Down, Key::Enter]);
	assert_eq!(commands(&mut app), [Command::ScanWifi]);
	app.set_networks(Vec::from([
		WifiNetwork {
			ssid: "weak".into(),
			signal: -80,
			secured: true,
		},
		WifiNetwork {
			ssid: "PHOTON".into(),
			signal: -50,
			secured: true,
		},
	]));
	press(&mut app, &[Key::Enter]);
	// Password, then back out and save without a peer, which is refused
	press(&mut app, &[Key::Down, Key::Enter]);
	type_text(&mut app, "secret");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("••••••"));
	press(&mut app, &[Key::Escape]);
	assert!(screen(&app).join("\n").contains("Bridge       on"));
	press(&mut app, &[Key::Down, Key::Down, Key::Enter]);
	assert!(commands(&mut app).is_empty());
	assert!(screen(&app).join("\n").contains("needs a network and peer"));
	// Peer
	press(&mut app, &[Key::Up, Key::Up, Key::Enter]);
	press(&mut app, &[Key::Down; 3]);
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "192.168.1.176:9001");
	press(&mut app, &[Key::Enter, Key::Escape]);
	// The hotspot, with a password too short for WPA2
	press(&mut app, &[Key::Down, Key::Enter]);
	assert!(screen(&app)[0].contains("Hotspot"));
	press(&mut app, &[Key::Enter, Key::Down, Key::Enter]);
	type_text(&mut app, "pocket");
	press(&mut app, &[Key::Enter, Key::Down, Key::Enter]);
	type_text(&mut app, "short");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("8 or more"));
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "hunter22");
	press(&mut app, &[Key::Enter, Key::Escape, Key::Down, Key::Enter]);
	let expected = Settings {
		name: "kitchen".into(),
		bridge: true,
		ssid: "PHOTON".into(),
		password: "secret".into(),
		peer: "192.168.1.176:9001".into(),
		channel: 1,
		status_page: false,
		utc_offset: -300,
		hotspot: true,
		hotspot_name: "pocket".into(),
		hotspot_password: "hunter22".into(),
	};
	assert_eq!(commands(&mut app), [Command::Save(expected)]);
}

#[test]
fn draws_every_screen_within_the_display() {
	let mut app = App::new(Settings::new(), at(0));
	let peer = key(2);
	app.set_status(status(&[peer], &[peer, key(3)]));
	assert!(screen(&app)[1].contains(&address(&key(1)).to_string()));
	for keys in [
		&[Key::Enter][..],
		&[Key::Enter],
		&[Key::Escape, Key::Escape, Key::Down, Key::Enter],
		&[Key::Escape, Key::Down, Key::Enter],
		&[Key::Escape, Key::Down, Key::Enter],
		&[Key::Down, Key::Down, Key::Enter],
	] {
		press(&mut app, keys);
		let shown = screen(&app);
		assert_eq!(shown.len(), 13);
	}
}

#[test]
fn shows_signal_and_loss_for_each_neighbor() {
	let mut app = App::new(Settings::new(), at(0));
	let (named, unnamed) = (key(2), key(3));
	let mut status = status(&[named, unnamed], &[named, unnamed]);
	status.neighbors[0].signal = Some(-48);
	status.neighbors[0].loss = Some(3);
	status.neighbors[1].signal = Some(-81);
	status.neighbors.push(Neighbor {
		mac: [0x28, 0x84, 0x85, 0x76, 0x2d, 0x64],
		key: None,
		signal: Some(-70),
		loss: None,
	});
	app.set_status(status);
	open(&mut app, "ESP-NOW Neighbors");
	app.tick(at(1));
	app.handle_nodeinfo(named, Some(br#"{"name":"kitchen"}"#.to_vec()));
	let shown = screen(&app).join("\n");
	assert!(
		shown.contains("kitchen           ▮▮▮▮  -48 dBm   3%"),
		"{shown}"
	);
	assert!(shown.contains("▮▮▮▮  -81 dBm"), "{shown}");
	assert!(shown.contains("28:84:85:76:2d:64"), "{shown}");
	assert!(shown.contains("linking"), "{shown}");
}

#[test]
fn shows_the_local_time_once_known() {
	let mut settings = Settings::new();
	settings.utc_offset = -300;
	let mut app = App::new(settings, at(0));
	let mut status = status(&[], &[]);
	assert!(!screen(&app)[0].contains(':'));
	// 2026-11-09 18:30 UTC
	status.board.time = Some(1_794_249_000_000_000);
	app.set_status(status);
	assert!(screen(&app)[0].contains("13:30"), "{}", screen(&app)[0]);
}

#[test]
fn typing_replaces_a_field_and_backspace_edits_it() {
	let mut settings = Settings::new();
	settings.channel = 11;
	let mut app = App::new(settings, at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "Settings");
	press(&mut app, &[Key::Down, Key::Enter]);
	type_text(&mut app, "6");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("Channel      6"));
	press(&mut app, &[Key::Enter, Key::Backspace]);
	type_text(&mut app, "3");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("Channel      3"));
	press(&mut app, &[Key::Enter, Key::Backspace, Key::Backspace]);
	type_text(&mut app, "12");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("Channel      12"));
}

#[test]
fn browses_gopher_menus_and_goes_back() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "Gopher");
	type_text(&mut app, "200::1");
	press(&mut app, &[Key::Enter]);
	let server = SocketAddrV6::new("200::1".parse().unwrap(), 70, 0, 0);
	assert_eq!(
		commands(&mut app),
		[Command::Fetch(server, b"\r\n".to_vec())]
	);

	// The menu names its own server, as menus usually do.
	let menu = "iWelcome to the board\t\terror.host\t1\r\n\
		0About\t/about.txt\tboard.local\t70\r\n\
		1Phlog\t/phlog\tboard.local\t70\r\n\
		1Elsewhere\t\tother.example\t70\r\n.\r\n";
	app.handle_event(Event::Fetched {
		data: menu.as_bytes().to_vec(),
	});
	assert!(screen(&app).join("\n").contains("Welcome to the board"));

	press(&mut app, &[Key::Enter]);
	assert_eq!(
		commands(&mut app),
		[Command::Fetch(server, b"/about.txt\r\n".to_vec())]
	);
	app.handle_event(Event::Fetched {
		data: b"Made of Rust.\r\n.\r\n".to_vec(),
	});
	assert!(screen(&app).join("\n").contains("Made of Rust."));

	press(&mut app, &[Key::Escape, Key::Down, Key::Down, Key::Enter]);
	assert!(commands(&mut app).is_empty());
	assert!(
		screen(&app)
			.join("\n")
			.contains("Can't look up other.example")
	);
	press(&mut app, &[Key::Escape]);
	// Out of history, so back to the apps menu.
	assert_eq!(screen(&app)[0].trim(), "esp-ygg / Apps");
}

fn irc_line(app: &mut App, line: &str) {
	let mut data = line.as_bytes().to_vec();
	data.extend_from_slice(b"\r\n");
	app.handle_event(Event::StreamData {
		stream: Stream::Irc,
		data,
	});
}

fn sent_lines(app: &mut App) -> Vec<String> {
	commands(app)
		.into_iter()
		.filter_map(|command| match command {
			Command::StreamSend(Stream::Irc, line) => Some(String::from_utf8(line).unwrap()),
			_ => None,
		})
		.collect()
}

#[test]
fn chats_on_irc() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "IRC");
	type_text(&mut app, "200::5");
	press(&mut app, &[Key::Enter, Key::Enter, Key::Enter]);
	let server = SocketAddrV6::new("200::5".parse().unwrap(), 6667, 0, 0);
	assert_eq!(
		commands(&mut app),
		[Command::OpenStream(Stream::Irc, server)]
	);

	app.handle_event(Event::StreamOpened(Stream::Irc));
	assert_eq!(
		sent_lines(&mut app),
		["NICK esp-ygg\r\n", "USER esp-ygg 0 * :esp-ygg\r\n"]
	);
	irc_line(
		&mut app,
		":irc.test 433 * esp-ygg :Nickname is already in use",
	);
	assert_eq!(sent_lines(&mut app), ["NICK esp-ygg_\r\n"]);
	irc_line(&mut app, ":irc.test 001 esp-ygg_ :Welcome to the mesh");
	assert_eq!(sent_lines(&mut app), ["JOIN #esp-ygg\r\n"]);
	irc_line(&mut app, ":esp-ygg_!u@200::1 JOIN #esp-ygg");
	irc_line(&mut app, "PING :irc.test");
	assert_eq!(sent_lines(&mut app), ["PONG :irc.test\r\n"]);

	irc_line(
		&mut app,
		":ada!a@200::2 PRIVMSG #esp-ygg :hello from the desktop",
	);
	type_text(&mut app, "hi there");
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "/me waves");
	press(&mut app, &[Key::Enter]);
	assert_eq!(
		sent_lines(&mut app),
		[
			"PRIVMSG #esp-ygg :hi there\r\n",
			"PRIVMSG #esp-ygg :\u{1}ACTION waves\u{1}\r\n"
		]
	);
	let shown = screen(&app).join("\n");
	assert!(shown.contains("#esp-ygg as esp-ygg_"), "{shown}");
	assert!(shown.contains("<ada> hello from the desktop"), "{shown}");
	assert!(shown.contains("* esp-ygg_ waves"), "{shown}");

	// A private message gets its own buffer, counted as unread.
	irc_line(&mut app, ":bob!b@200::3 PRIVMSG esp-ygg_ :psst");
	assert!(screen(&app)[1].contains("bob(1)"), "{}", screen(&app)[1]);
	press(&mut app, &[Key::Escape]);
	irc_line(&mut app, ":ada!a@200::2 PRIVMSG #esp-ygg :still there?");
	assert!(screen(&app).join("\n").contains("IRC (2 new)"));
}

#[test]
fn previews_a_time_zone_until_settings_are_left() {
	let mut app = App::new(Settings::new(), at(0));
	let mut status = status(&[], &[]);
	// 2026-11-09 18:30 UTC
	status.board.time = Some(1_794_249_000_000_000);
	app.set_status(status);
	open(&mut app, "Settings");
	let time_zone = FIELDS
		.iter()
		.position(|field| *field == Field::TimeZone)
		.unwrap();
	for _ in 0..time_zone {
		press(&mut app, &[Key::Down]);
	}
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "-5");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app)[0].contains("13:30"), "{}", screen(&app)[0]);
	// Leaving without saving drops the change.
	press(&mut app, &[Key::Escape]);
	assert!(screen(&app)[0].contains("18:30"), "{}", screen(&app)[0]);
}

/// Carries the app's Gemini connections to servers that answer each
/// request with what `respond` gives, and returns the requests.
fn serve_gemini(app: &mut App, respond: impl Fn(&str) -> &'static str) -> Vec<String> {
	use std::io::{Read, Write};
	let mut requests = Vec::new();
	let mut server = None;
	loop {
		let pending = commands(app);
		if pending.is_empty() {
			return requests;
		}
		for command in pending {
			match command {
				Command::OpenStream(Stream::Gemini, to) => {
					assert_eq!(to.port(), 1965);
					server = Some(crate::tls::tests::server(&[&rustls::version::TLS13]));
					app.handle_event(Event::StreamOpened(Stream::Gemini));
				}
				Command::StreamSend(Stream::Gemini, data) => {
					let server = server.as_mut().expect("sent before opening");
					server.read_tls(&mut &data[..]).unwrap();
					server.process_new_packets().unwrap();
					let mut request = Vec::new();
					let _ = server.reader().read_to_end(&mut request);
					if !request.is_empty() {
						let request = String::from_utf8(request).unwrap();
						let response = respond(request.trim_end());
						requests.push(request.trim_end().into());
						server.writer().write_all(response.as_bytes()).unwrap();
						server.send_close_notify();
					}
					let mut reply = Vec::new();
					while server.wants_write() {
						server.write_tls(&mut reply).unwrap();
					}
					if !reply.is_empty() {
						app.handle_event(Event::StreamData {
							stream: Stream::Gemini,
							data: reply,
						});
					}
				}
				// The browser is done with the connection by the time it closes it.
				Command::CloseStream(Stream::Gemini) => server = None,
				other => panic!("unexpected {other:?}"),
			}
		}
	}
}

#[test]
fn browses_gemini_capsules() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	open(&mut app, "Gemini");
	type_text(&mut app, "200::1");
	press(&mut app, &[Key::Enter]);
	let respond = |request: &str| match request {
		"gemini://[200::1]/" => {
			"20 text/gemini\r\n# Capsule\nHello\n=> /next Next\n=> https://example.org Web\n"
		}
		"gemini://[200::1]/next" => "31 /ask\r\n",
		"gemini://[200::1]/ask" => "10 Your name?\r\n",
		"gemini://[200::1]/ask?a%20b" => "20 text/plain\r\nHi a b\n",
		_ => "51 Not found\r\n",
	};
	assert_eq!(serve_gemini(&mut app, respond), ["gemini://[200::1]/"]);
	let shown = screen(&app).join("\n");
	assert!(
		shown.contains("Capsule") && shown.contains("→ Next"),
		"{shown}"
	);

	press(&mut app, &[Key::Down, Key::Enter]);
	assert!(screen(&app).join("\n").contains("Can't open https links"));

	press(&mut app, &[Key::Up, Key::Enter]);
	assert_eq!(
		serve_gemini(&mut app, respond),
		["gemini://[200::1]/next", "gemini://[200::1]/ask"]
	);
	assert!(screen(&app).join("\n").contains("Your name?"));
	type_text(&mut app, "a b");
	press(&mut app, &[Key::Enter]);
	assert_eq!(
		serve_gemini(&mut app, respond),
		["gemini://[200::1]/ask?a%20b"]
	);
	assert!(screen(&app).join("\n").contains("Hi a b"));

	// Back goes to the page before, fetching it again.
	press(&mut app, &[Key::Escape]);
	assert_eq!(serve_gemini(&mut app, respond), ["gemini://[200::1]/"]);
	assert!(screen(&app).join("\n").contains("Capsule"));
}

#[test]
fn completes_addresses_of_known_nodes_with_tab() {
	let mut app = App::new(Settings::new(), at(0));
	let (kitchen, other) = (key(2), key(3));
	app.set_status(status(&[kitchen], &[kitchen, other]));
	open(&mut app, "UDP Console");
	app.tick(at(1));
	assert_eq!(commands(&mut app).len(), 2, "names are looked up");
	app.handle_nodeinfo(kitchen, Some(br#"{"name":"Kitchen"}"#.to_vec()));
	app.handle_nodeinfo(other, Some(b"{}".to_vec()));

	// Every Yggdrasil address starts with 2, so Tab cycles through both.
	type_text(&mut app, "2");
	press(&mut app, &[Key::Tab]);
	assert_eq!(app.console.target.text, address(&kitchen).to_string());
	assert!(screen(&app).join("\n").contains("Kitchen  1 of 2"));
	press(&mut app, &[Key::Tab]);
	assert_eq!(app.console.target.text, address(&other).to_string());
	press(&mut app, &[Key::Tab]);
	assert_eq!(app.console.target.text, address(&kitchen).to_string());

	// A name works too, and Tab on a whole address moves on as before.
	app.console.target.clear();
	type_text(&mut app, "kit");
	press(&mut app, &[Key::Tab]);
	assert_eq!(app.console.target.text, address(&kitchen).to_string());
	press(&mut app, &[Key::Tab]);
	assert_eq!(app.console.focus, console::Focus::Message);
}

#[test]
fn home_menu_follows_the_selection() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	press(&mut app, &[Key::Down; 20]);
	let shown = screen(&app).join("\n");
	let (last, _) = MENU[MENU.len() - 1];
	assert!(shown.contains(&format!(" {last}")), "{shown}");
	assert!(shown.contains(&address(&key(1)).to_string()));
}

#[cfg(feature = "voice")]
#[test]
fn takes_and_places_calls() {
	use crate::voice::iax2::CallEvent;

	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	// An incoming call brings up the screen from anywhere.
	let caller = address(&key(2));
	app.handle_event(Event::Call(CallEvent::Incoming {
		from: caller,
		name: Some("kitchen".into()),
	}));
	assert!(screen(&app).join("\n").contains("kitchen"));
	press(&mut app, &[Key::Enter]);
	assert_eq!(commands(&mut app), [Command::Answer]);
	app.set_talking(true);
	assert!(screen(&app).join("\n").contains("Talking"));
	app.handle_event(Event::Call(CallEvent::Ended(
		"The other side hung up".into(),
	)));
	assert!(screen(&app).join("\n").contains("The other side hung up"));

	type_text(&mut app, &caller.to_string());
	press(&mut app, &[Key::Enter]);
	assert_eq!(commands(&mut app), [Command::Call(caller)]);
	app.handle_event(Event::Call(CallEvent::Ringing));
	assert!(screen(&app).join("\n").contains("Ringing"));
	press(&mut app, &[Key::Escape]);
	assert_eq!(commands(&mut app), [Command::HangUp]);
}
