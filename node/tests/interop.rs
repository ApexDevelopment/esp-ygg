//! A simulated ESP-NOW mesh whose first node also peers over TCP with a
//! yggdrasil-rs node, standing in for a desktop on the wider network.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv6Addr, TcpStream};
use std::time::{Duration, Instant};

use yggdrasil::core::{Core, Options};
use yggdrasil::ipv6rwc::ReadWriteCloser;
use yggdrasil_espnow::sim::{Radio, echo_request, is_echo_reply, private_key};
use yggdrasil_espnow::yggdrasil_proto::requests::build_nodeinfo;
use yggdrasil_espnow::{Config, PublicKey, StreamId};

const GATEWAY: usize = 0;
const FAR: usize = 1;

fn hex(key: &PublicKey) -> String {
	key.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Moves bytes between the gateway node and its TCP socket.
fn pump_stream(radio: &mut Radio, id: StreamId, socket: &mut TcpStream) {
	let mut buf = [0u8; 4096];
	loop {
		match socket.read(&mut buf) {
			Ok(0) => panic!("desktop closed the connection"),
			Ok(n) => radio.nodes[GATEWAY].stream_receive(id, &buf[..n]).unwrap(),
			Err(error) if error.kind() == ErrorKind::WouldBlock => break,
			Err(error) => panic!("read failed: {error}"),
		}
	}
	loop {
		let pending = radio.nodes[GATEWAY].stream_transmit(id);
		if pending.is_empty() {
			break;
		}
		match socket.write(pending) {
			Ok(n) => radio.nodes[GATEWAY].stream_consume(id, n),
			Err(error) if error.kind() == ErrorKind::WouldBlock => break,
			Err(error) => panic!("write failed: {error}"),
		}
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_reaches_a_node_behind_an_espnow_gateway() {
	let desktop = Core::new(private_key(200), Options::default())
		.await
		.unwrap();
	let listener = desktop.listen("tcp://127.0.0.1:0", "").await.unwrap();
	let desktop_rwc = ReadWriteCloser::new(desktop.clone());

	let mut radio = Radio::new();
	radio.add_node(Config::new(private_key(1)));
	let mut far_config = Config::new(private_key(2));
	let given = serde_json::json!({ "name": "far" });
	far_config.nodeinfo = build_nodeinfo(given.as_object().unwrap(), None).unwrap();
	radio.add_node(far_config);
	radio.connect(GATEWAY, FAR);

	let mut socket = TcpStream::connect(listener.local_addr()).unwrap();
	socket.set_nonblocking(true).unwrap();
	let stream = radio.nodes[GATEWAY].open_stream();

	let far_key = radio.nodes[FAR].public_key();
	let far_address = Ipv6Addr::from(radio.nodes[FAR].address().0);
	let desktop_address = Ipv6Addr::from(desktop.address().0);

	// The desktop pings the far node and asks for its nodeinfo from tasks,
	// while this one drives the simulation in step with the real clock.
	let ping = tokio::spawn({
		let rwc = desktop_rwc.clone();
		async move {
			let request = echo_request(far_address, desktop_address, b"over the air");
			let mut buf = vec![0u8; 2048];
			loop {
				rwc.write(&request).unwrap();
				let reply = tokio::time::timeout(Duration::from_secs(1), rwc.read(&mut buf)).await;
				if let Ok(Ok(n)) = reply
					&& is_echo_reply(&buf[..n], far_address, b"over the air")
				{
					return;
				}
			}
		}
	});
	let nodeinfo = tokio::spawn({
		let desktop = desktop.clone();
		let key = hex(&far_key);
		async move {
			loop {
				if let Ok(info) = desktop.get_node_info(&key).await {
					return info;
				}
			}
		}
	});

	let started = Instant::now();
	while !(ping.is_finished() && nodeinfo.is_finished()) {
		assert!(started.elapsed() < Duration::from_secs(60), "timed out");
		radio.set_clock(started.elapsed().as_micros() as u64);
		radio.step();
		pump_stream(&mut radio, stream, &mut socket);
		tokio::time::sleep(Duration::from_millis(1)).await;
	}
	ping.await.unwrap();
	let info = nodeinfo.await.unwrap();
	assert_eq!(info[hex(&far_key)]["name"], "far");

	// And the other way: the far node's packets reach the desktop.
	let request = echo_request(desktop_address, far_address, b"from the mesh");
	let received = tokio::spawn({
		let rwc = desktop_rwc.clone();
		async move {
			let mut buf = vec![0u8; 2048];
			loop {
				let n = rwc.read(&mut buf).await.unwrap();
				if buf[8..24] == far_address.octets() && buf[48..n] == *b"from the mesh" {
					return;
				}
			}
		}
	});
	let mut last_sent = None;
	while !received.is_finished() {
		assert!(started.elapsed() < Duration::from_secs(90), "timed out");
		if last_sent.is_none_or(|sent: Instant| sent.elapsed() > Duration::from_secs(1)) {
			radio.nodes[FAR].send_ipv6(&request).unwrap();
			last_sent = Some(Instant::now());
		}
		radio.set_clock(started.elapsed().as_micros() as u64);
		radio.step();
		pump_stream(&mut radio, stream, &mut socket);
		tokio::time::sleep(Duration::from_millis(1)).await;
	}
	desktop.stop();
}
