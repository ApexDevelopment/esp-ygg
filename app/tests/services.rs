//! Runs the services between simulated boards.

use std::net::{Ipv6Addr, SocketAddrV6};
use std::time::Duration;

use esp_ygg_app::device::{self, Queues};
use esp_ygg_app::{BoardInfo, CONSOLE_PORT, Event, HTTP_PORT, Overlay, Stream};
use smoltcp::iface::SocketSet;
use smoltcp::socket::tcp;
use smoltcp::wire::IpAddress;
use yggdrasil_espnow::sim::Radio;

fn board() -> BoardInfo {
	BoardInfo {
		name: "Test <board>".into(),
		model: "Simulated".into(),
		uptime: Duration::from_secs(90),
		free_heap: Some(100 * 1024),
		time: None,
	}
}

fn address(radio: &Radio, index: usize) -> Ipv6Addr {
	Ipv6Addr::from(radio.nodes[index].address().0)
}

/// A line of peered boards, with an overlay on each board in `with_overlay`.
fn mesh(boards: usize, with_overlay: &[usize]) -> (Radio, Vec<(usize, Overlay)>) {
	mesh_with(boards, with_overlay, true)
}

fn mesh_with(
	boards: usize,
	with_overlay: &[usize],
	status_page: bool,
) -> (Radio, Vec<(usize, Overlay)>) {
	let mut radio = Radio::line(boards);
	assert!(radio.run_until(Duration::from_secs(20), |radio| radio.all_peered()));
	let overlays = with_overlay
		.iter()
		.map(|&index| {
			(
				index,
				Overlay::new(&radio.nodes[index], index as u64 + 1, status_page),
			)
		})
		.collect();
	(radio, overlays)
}

/// Advances one millisecond and polls every overlay.
fn tick(radio: &mut Radio, overlays: &mut [(usize, Overlay)]) {
	radio.run(Duration::from_millis(1));
	for (index, overlay) in overlays.iter_mut() {
		overlay.poll(&mut radio.nodes[*index], &board());
	}
}

/// Ticks until `found` returns something from an overlay's events.
fn wait_for<T>(
	radio: &mut Radio,
	overlays: &mut [(usize, Overlay)],
	limit: Duration,
	mut found: impl FnMut(&mut Radio, &mut [(usize, Overlay)]) -> Option<T>,
) -> Option<T> {
	for _ in 0..limit.as_millis() {
		tick(radio, overlays);
		if let Some(value) = found(radio, overlays) {
			return Some(value);
		}
	}
	None
}

#[test]
fn pings_across_the_mesh() {
	let (mut radio, mut overlays) = mesh(3, &[0, 2]);
	let target = address(&radio, 2);
	let reply = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|radio, overlays| {
			let overlay = &mut overlays[0].1;
			// The first pings may time out while the route is found.
			if radio.now_micros() % 1_000_000 < 1_000 {
				overlay.ping(target, radio.nodes[0].now()).unwrap();
			}
			while let Some(event) = overlay.poll_event() {
				if let Event::PingReply {
					from, round_trip, ..
				} = event
				{
					return Some((from, round_trip));
				}
			}
			None
		},
	);
	let (from, round_trip) = reply.expect("no ping reply");
	assert_eq!(from, target);
	assert!(round_trip < Duration::from_secs(1));
}

#[test]
fn datagrams_reach_the_console() {
	let (mut radio, mut overlays) = mesh(3, &[0, 2]);
	let destination = SocketAddrV6::new(address(&radio, 2), CONSOLE_PORT, 0, 0);
	let source = address(&radio, 0);
	let received = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|radio, overlays| {
			if radio.now_micros() % 1_000_000 < 1_000 {
				overlays[0].1.send_datagram(destination, b"hello").unwrap();
			}
			match overlays[1].1.poll_event() {
				Some(Event::Datagram { from, data }) => Some((from, data)),
				_ => None,
			}
		},
	);
	let (from, data) = received.expect("no datagram");
	assert_eq!(*from.ip(), source);
	assert_eq!(from.port(), CONSOLE_PORT);
	assert_eq!(data, b"hello");
}

/// Fetches `path` from board 0 with a TCP client on board 1. Returns
/// everything received before the connection closed.
fn fetch(path: &str) -> String {
	fetch_from(path, true)
}

fn fetch_from(path: &str, status_page: bool) -> String {
	let (mut radio, mut overlays) = mesh_with(2, &[0], status_page);
	let server = address(&radio, 0);
	let mut queues = Queues::default();
	let now = smoltcp::time::Instant::from_micros(radio.now_micros() as i64);
	let mut interface = device::interface(&radio.nodes[1].address(), &mut queues, 7, now);
	let mut sockets = SocketSet::new(Vec::new());
	let client = sockets.add(tcp::Socket::new(
		tcp::SocketBuffer::new(vec![0; 8192]),
		tcp::SocketBuffer::new(vec![0; 1024]),
	));
	let socket = sockets.get_mut::<tcp::Socket>(client);
	socket
		.connect(
			interface.context(),
			(IpAddress::Ipv6(server), HTTP_PORT),
			49152,
		)
		.unwrap();
	let request = format!("GET {path} HTTP/1.0\r\nHost: board\r\n\r\n");
	let mut sent = false;
	let mut response = Vec::new();
	let done = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(60),
		|radio, _| {
			while let Some(packet) = radio.nodes[1].recv_ipv6() {
				queues.received.push_back(packet);
			}
			let now = smoltcp::time::Instant::from_micros(radio.now_micros() as i64);
			interface.poll(now, &mut queues, &mut sockets);
			let socket = sockets.get_mut::<tcp::Socket>(client);
			if socket.may_send() && !sent {
				socket.send_slice(request.as_bytes()).unwrap();
				sent = true;
			}
			while socket.can_recv() {
				socket
					.recv(|data| {
						response.extend_from_slice(data);
						(data.len(), ())
					})
					.unwrap();
			}
			let finished = (sent && !socket.may_recv()) || !socket.is_open();
			interface.poll(now, &mut queues, &mut sockets);
			while let Some(packet) = queues.outgoing.pop_front() {
				let _ = radio.nodes[1].send_ipv6(&packet);
			}
			finished.then_some(())
		},
	);
	assert!(done.is_some(), "the request did not finish");
	String::from_utf8(response).unwrap()
}

#[test]
fn serves_the_status_page() {
	let page = fetch("/");
	assert!(page.starts_with("HTTP/1.0 200 OK\r\n"), "{page}");
	assert!(page.contains("<h1>Test &lt;board&gt;</h1>"));
	assert!(page.contains("1 min 30 s"));
	assert!(page.contains("<h2>Peers</h2>"));
}

#[test]
fn answers_unknown_paths_with_not_found() {
	assert!(fetch("/missing").starts_with("HTTP/1.0 404 Not Found\r\n"));
}

#[test]
fn a_board_reaches_itself_at_once() {
	let (mut radio, mut overlays) = mesh(2, &[0]);
	let own = address(&radio, 0);
	let overlay = &mut overlays[0].1;
	overlay.ping(own, radio.nodes[0].now()).unwrap();
	let console = SocketAddrV6::new(own, CONSOLE_PORT, 0, 0);
	overlay.send_datagram(console, b"hello").unwrap();
	tick(&mut radio, &mut overlays);
	let events: Vec<Event> = std::iter::from_fn(|| overlays[0].1.poll_event()).collect();
	assert!(
		events
			.iter()
			.any(|event| matches!(event, Event::PingReply { from, .. } if *from == own))
	);
	assert!(
		events
			.iter()
			.any(|event| matches!(event, Event::Datagram { data, .. } if data == b"hello"))
	);
}

#[test]
fn pings_to_nobody_time_out() {
	let (mut radio, mut overlays) = mesh(2, &[0]);
	let nobody: Ipv6Addr = "200:1234::1".parse().unwrap();
	let sequence = overlays[0].1.ping(nobody, radio.nodes[0].now()).unwrap();
	let event = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(10),
		|_, overlays| overlays[0].1.poll_event(),
	);
	assert_eq!(
		event,
		Some(Event::PingTimeout {
			target: nobody,
			sequence
		})
	);
}

#[test]
fn the_status_page_can_be_turned_off() {
	assert_eq!(fetch_from("/", false), "");
}

#[test]
fn time_spreads_from_board_to_board() {
	let (mut radio, mut overlays) = mesh(3, &[0, 1, 2]);
	let start = radio.nodes[0].now();
	let reference = 1_800_000_000_000_000;
	overlays[0].1.set_time(reference, 2, start);
	let synced = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(120),
		|radio, overlays| {
			let now = radio.nodes[2].now();
			overlays[2].1.unix_micros(now).map(|time| (time, now))
		},
	);
	let (time, now) = synced.expect("the far board never learned the time");
	let expected = reference + now.saturating_duration_since(start).as_micros() as u64;
	// Each hop adds the time a reply takes to arrive.
	assert!(time.abs_diff(expected) < 50_000, "{time} vs {expected}");
	// The source keeps its own time rather than taking a neighbor's.
	let now = radio.nodes[0].now();
	let expected = reference + now.saturating_duration_since(start).as_micros() as u64;
	assert_eq!(overlays[0].1.unix_micros(now), Some(expected));
}

/// Fetches from board 0 with board 1's overlay, and returns the outcome.
fn fetch_with_overlay(port: u16, request: &[u8]) -> Event {
	let (mut radio, mut overlays) = mesh(2, &[0, 1]);
	let server = SocketAddrV6::new(address(&radio, 0), port, 0, 0);
	let now = radio.nodes[1].now();
	overlays[1].1.fetch(server, request.to_vec(), now).unwrap();
	wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|_, overlays| overlays[1].1.poll_event(),
	)
	.expect("the fetch never finished")
}

#[test]
fn boards_serve_a_gopher_menu_linking_their_peers() {
	let Event::Fetched { data } = fetch_with_overlay(esp_ygg_app::gopher::PORT, b"\r\n") else {
		panic!("the fetch failed");
	};
	let items = esp_ygg_app::gopher::parse_menu(&data);
	assert_eq!(items[0].text, "Test <board>");
	let peer = items
		.iter()
		.find(|item| item.kind == '1')
		.expect("no peer link");
	assert_eq!(peer.port, 70);
	assert!(peer.url().address().is_some());
}

#[test]
fn fetching_from_a_closed_port_fails() {
	assert_eq!(fetch_with_overlay(7, b"\r\n"), Event::FetchFailed);
}

#[test]
fn streams_carry_data_both_ways() {
	let (mut radio, mut overlays) = mesh(2, &[0, 1]);
	let server = SocketAddrV6::new(address(&radio, 0), esp_ygg_app::gopher::PORT, 0, 0);
	overlays[1].1.open_stream(Stream::Irc, server).unwrap();
	let mut received = Vec::new();
	let closed = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|_, overlays| {
			while let Some(event) = overlays[1].1.poll_event() {
				match event {
					Event::StreamOpened(_) => {
						overlays[1].1.stream_send(Stream::Irc, b"\r\n").unwrap()
					}
					Event::StreamData { data, .. } => received.extend(data),
					Event::StreamClosed(_) => return Some(()),
					_ => {}
				}
			}
			None
		},
	);
	assert!(closed.is_some(), "the stream never closed");
	assert!(String::from_utf8_lossy(&received).contains("Test <board>"));
}

const PHONE_MAC: [u8; 6] = [0x8a, 0, 0, 0, 0, 2];
const BOARD_MAC: [u8; 6] = [0x24, 0, 0, 0, 0, 1];

/// An Ethernet frame from a phone on a hotspot. `checksum_at` is where in
/// `payload` its checksum goes.
fn phone_frame(
	from: Ipv6Addr,
	to: Ipv6Addr,
	next_header: u8,
	mut payload: Vec<u8>,
	checksum_at: usize,
) -> Vec<u8> {
	let length = payload.len() as u16;
	let mut pseudo = Vec::new();
	pseudo.extend_from_slice(&from.octets());
	pseudo.extend_from_slice(&to.octets());
	pseudo.extend_from_slice(&u32::from(length).to_be_bytes());
	pseudo.extend_from_slice(&[0, 0, 0, next_header]);
	pseudo.extend_from_slice(&payload);
	let mut sum: u32 = 0;
	for pair in pseudo.chunks(2) {
		sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
	}
	while sum > 0xffff {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	payload[checksum_at..checksum_at + 2].copy_from_slice(&(!(sum as u16)).to_be_bytes());
	let mut frame = Vec::from(BOARD_MAC);
	frame.extend_from_slice(&PHONE_MAC);
	frame.extend_from_slice(&[0x86, 0xdd, 0x60, 0, 0, 0]);
	frame.extend_from_slice(&length.to_be_bytes());
	frame.extend_from_slice(&[next_header, 64]);
	frame.extend_from_slice(&from.octets());
	frame.extend_from_slice(&to.octets());
	frame.extend_from_slice(&payload);
	frame
}

fn phone_datagram(from: SocketAddrV6, to: SocketAddrV6, data: &[u8]) -> Vec<u8> {
	let mut udp = Vec::new();
	udp.extend_from_slice(&from.port().to_be_bytes());
	udp.extend_from_slice(&to.port().to_be_bytes());
	udp.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
	udp.extend_from_slice(&[0, 0]);
	udp.extend_from_slice(data);
	phone_frame(*from.ip(), *to.ip(), 17, udp, 6)
}

fn phone_ping(from: Ipv6Addr, to: Ipv6Addr) -> Vec<u8> {
	let mut echo = Vec::from([128, 0, 0, 0, 0, 1, 0, 1]);
	echo.extend_from_slice(b"ping");
	phone_frame(from, to, 58, echo, 2)
}

#[test]
fn hotspot_devices_reach_the_mesh_and_hear_back() {
	let (mut radio, mut overlays) = mesh(3, &[0, 2]);
	overlays[0].1.start_hotspot(&radio.nodes[0], BOARD_MAC);
	let mut phone_ip = radio.nodes[0].subnet().0.to_vec();
	phone_ip.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 7]);
	let phone_ip = Ipv6Addr::from(<[u8; 16]>::try_from(phone_ip).unwrap());
	let phone = SocketAddrV6::new(phone_ip, 5000, 0, 0);
	let far_console = SocketAddrV6::new(address(&radio, 2), CONSOLE_PORT, 0, 0);
	let near_console = SocketAddrV6::new(address(&radio, 0), CONSOLE_PORT, 0, 0);

	let heard = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|radio, overlays| {
			if radio.now_micros() % 1_000_000 < 1_000 {
				let frame = phone_datagram(phone, far_console, b"from a phone");
				overlays[0]
					.1
					.receive_hotspot_frame(&mut radio.nodes[0], &frame);
			}
			match overlays[1].1.poll_event() {
				Some(Event::Datagram { from, data }) => Some((from, data)),
				_ => None,
			}
		},
	);
	assert_eq!(heard, Some((phone, b"from a phone".to_vec())));

	overlays[1].1.send_datagram(phone, b"hello phone").unwrap();
	let answer = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(10),
		|_, overlays| {
			std::iter::from_fn(|| overlays[0].1.poll_hotspot_frame())
				.find(|frame| frame.ends_with(b"hello phone"))
		},
	);
	assert_eq!(answer.expect("no answer reached the phone")[..6], PHONE_MAC);

	let frame = phone_datagram(phone, near_console, b"to this board");
	overlays[0]
		.1
		.receive_hotspot_frame(&mut radio.nodes[0], &frame);
	let local = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(1),
		|_, overlays| match overlays[0].1.poll_event() {
			Some(Event::Datagram { data, .. }) => Some(data),
			_ => None,
		},
	);
	assert_eq!(local, Some(b"to this board".to_vec()));

	let ping = phone_ping(phone_ip, address(&radio, 0));
	overlays[0]
		.1
		.receive_hotspot_frame(&mut radio.nodes[0], &ping);
	let reply = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(1),
		|_, overlays| overlays[0].1.poll_hotspot_frame(),
	);
	// An echo reply
	assert_eq!(reply.expect("no ping reply")[14 + 40], 129);
}

#[test]
fn boards_call_each_other_across_the_mesh() {
	use esp_ygg_app::voice::Codec;
	use esp_ygg_app::voice::iax2::CallEvent;

	let (mut radio, mut overlays) = mesh(3, &[0, 2]);
	overlays[0].1.start_phone("near", 1);
	overlays[1].1.start_phone("far", 2);
	let far = address(&radio, 2);
	let now = radio.nodes[0].now();
	overlays[0].1.call(far, now).unwrap();
	let incoming = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(30),
		|_, overlays| match overlays[1].1.poll_event() {
			Some(Event::Call(event @ CallEvent::Incoming { .. })) => Some(event),
			_ => None,
		},
	);
	assert_eq!(
		incoming,
		Some(CallEvent::Incoming {
			from: address(&radio, 0),
			name: Some("near".into())
		})
	);
	let now = radio.nodes[2].now();
	overlays[1].1.answer(now).unwrap();
	let answered = wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(5),
		|_, overlays| {
			std::iter::from_fn(|| overlays[0].1.poll_event())
				.find(|event| *event == Event::Call(CallEvent::Answered))
		},
	);
	assert!(answered.is_some(), "the call was not answered");

	let mut heard = Vec::new();
	for frame in 0..5u8 {
		let now = radio.nodes[0].now();
		overlays[0].1.send_voice(&[frame; 160], now);
		wait_for(
			&mut radio,
			&mut overlays,
			Duration::from_millis(20),
			|_, overlays| -> Option<()> {
				heard.extend(std::iter::from_fn(|| overlays[1].1.poll_voice()));
				None
			},
		);
	}
	wait_for(
		&mut radio,
		&mut overlays,
		Duration::from_secs(1),
		|_, overlays| -> Option<()> {
			heard.extend(std::iter::from_fn(|| overlays[1].1.poll_voice()));
			None
		},
	);
	assert_eq!(
		heard,
		(0..5u8)
			.map(|frame| (Codec::G726, vec![frame; 160]))
			.collect::<Vec<_>>()
	);
}
