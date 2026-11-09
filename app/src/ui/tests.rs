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

fn type_text(app: &mut App, text: &str) {
	for character in text.chars() {
		app.handle_key(Key::Char(character));
	}
}

/// Renders the app and returns the screen as lines of text.
fn screen(app: &App) -> Vec<String> {
	let mut terminal = Terminal::new(TestBackend::new(40, 13)).unwrap();
	terminal.draw(|frame| draw(frame, app)).unwrap();
	let buffer = terminal.backend().buffer();
	(0..13)
		.map(|y| (0..40).map(|x| buffer[(x, y)].symbol()).collect())
		.collect()
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
	press(&mut app, &[Key::Down, Key::Down, Key::Down, Key::Enter]);
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
	assert!(screen(&app).join("\n").contains("Console (1 new)"));
	press(
		&mut app,
		&[Key::Down, Key::Down, Key::Down, Key::Enter, Key::Escape],
	);
	assert!(!screen(&app).join("\n").contains("new"));
}

#[test]
fn edits_and_saves_bridge_settings() {
	let mut app = App::new(Settings::new(), at(0));
	app.set_status(status(&[], &[]));
	press(
		&mut app,
		&[
			Key::Up,
			Key::Down,
			Key::Down,
			Key::Down,
			Key::Down,
			Key::Enter,
		],
	);
	// Name
	press(&mut app, &[Key::Enter]);
	type_text(&mut app, "kitchen");
	press(&mut app, &[Key::Enter]);
	// Bridge on, then pick a network from a scan
	press(&mut app, &[Key::Down, Key::Enter, Key::Down, Key::Enter]);
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
	// Password, then save without a peer, which is refused
	press(&mut app, &[Key::Down, Key::Enter]);
	type_text(&mut app, "secret");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("••••••"));
	let to_save = [
		Key::Down,
		Key::Down,
		Key::Down,
		Key::Down,
		Key::Down,
		Key::Enter,
	];
	press(&mut app, &to_save);
	assert!(commands(&mut app).is_empty());
	assert!(screen(&app).join("\n").contains("needs a network and peer"));
	// Peer, a time zone, the status page off, then save
	press(&mut app, &[Key::Up, Key::Up, Key::Up, Key::Up, Key::Enter]);
	type_text(&mut app, "192.168.1.176:9001");
	press(&mut app, &[Key::Enter, Key::Down, Key::Down, Key::Enter]);
	for _ in 0..6 {
		press(&mut app, &[Key::Backspace]);
	}
	type_text(&mut app, "-5");
	press(&mut app, &[Key::Enter]);
	assert!(screen(&app).join("\n").contains("UTC-05:00"));
	press(&mut app, &[Key::Down, Key::Enter]);
	assert!(screen(&app).join("\n").contains("Web page off"));
	press(&mut app, &[Key::Down, Key::Enter]);
	let expected = Settings {
		name: "kitchen".into(),
		bridge: true,
		ssid: "PHOTON".into(),
		password: "secret".into(),
		peer: "192.168.1.176:9001".into(),
		channel: 1,
		status_page: false,
		utc_offset: -300,
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
	press(&mut app, &[Key::Down, Key::Enter]);
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
