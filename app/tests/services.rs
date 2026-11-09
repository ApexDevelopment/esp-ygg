//! Runs the services between simulated boards.

use std::net::{Ipv6Addr, SocketAddrV6};
use std::time::Duration;

use esp_ygg_app::device::{self, Queues};
use esp_ygg_app::{BoardInfo, CONSOLE_PORT, Event, HTTP_PORT, Overlay};
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
