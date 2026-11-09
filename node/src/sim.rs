//! A simulated ESP-NOW radio shared by several nodes, for tests and desktop
//! previews. Enabled by the `sim` feature.

use std::collections::BTreeMap;
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::vec;
use std::vec::Vec;

use yggdrasil_proto::icmpv6::create_icmpv6;

use crate::{Config, Destination, FRAME_LEN, Instant, Mac, Node, Platform, PrivateKey};

const LATENCY_MICROS: u64 = 1_000;

#[derive(Clone)]
pub struct SimPlatform {
	clock_micros: Arc<AtomicU64>,
	rng: u64,
}

impl Platform for SimPlatform {
	fn now(&self) -> Instant {
		Instant::from_duration(Duration::from_micros(
			self.clock_micros.load(Ordering::Relaxed),
		))
	}

	fn unix_time(&self) -> u64 {
		1_800_000_000 + self.clock_micros.load(Ordering::Relaxed) / 1_000_000
	}

	fn fill_random(&mut self, buf: &mut [u8]) {
		for byte in buf {
			self.rng ^= self.rng << 13;
			self.rng ^= self.rng >> 7;
			self.rng ^= self.rng << 17;
			*byte = self.rng as u8;
		}
	}
}

pub fn private_key(seed: u8) -> PrivateKey {
	PrivateKey::from_seed(&[seed; 32])
}

pub struct Radio {
	clock_micros: Arc<AtomicU64>,
	pub nodes: Vec<Node<SimPlatform>>,
	macs: Vec<Mac>,
	/// Which nodes can hear each other.
	in_range: Vec<Vec<bool>>,
	air: BTreeMap<(u64, u64), (usize, Mac, Vec<u8>)>,
	sent: u64,
	/// Every `loss_every`-th frame is lost, if set.
	pub loss_every: Option<u64>,
}

impl Default for Radio {
	fn default() -> Self {
		Self::new()
	}
}

impl Radio {
	pub fn new() -> Self {
		Radio {
			clock_micros: Arc::new(AtomicU64::new(0)),
			nodes: Vec::new(),
			macs: Vec::new(),
			in_range: Vec::new(),
			air: BTreeMap::new(),
			sent: 0,
			loss_every: None,
		}
	}

	pub fn add_node(&mut self, config: Config) -> usize {
		let index = self.nodes.len();
		let platform = SimPlatform {
			clock_micros: self.clock_micros.clone(),
			rng: 0x9e37_79b9_7f4a_7c15 ^ (index as u64 + 1),
		};
		self.nodes.push(Node::new(config, platform).unwrap());
		self.macs.push([0x02, 0, 0, 0, 0, index as u8]);
		for row in &mut self.in_range {
			row.push(false);
		}
		self.in_range.push(vec![false; index + 1]);
		index
	}

	pub fn connect(&mut self, a: usize, b: usize) {
		self.in_range[a][b] = true;
		self.in_range[b][a] = true;
	}

	pub fn disconnect(&mut self, a: usize, b: usize) {
		self.in_range[a][b] = false;
		self.in_range[b][a] = false;
	}

	/// Builds `nodes` nodes in a line, each in range of the next.
	pub fn line(nodes: usize) -> Self {
		let mut radio = Radio::new();
		for index in 0..nodes {
			radio.add_node(Config::new(private_key(index as u8 + 1)));
		}
		for index in 1..nodes {
			radio.connect(index - 1, index);
		}
		radio
	}

	pub fn now_micros(&self) -> u64 {
		self.clock_micros.load(Ordering::Relaxed)
	}

	fn transmit(&mut self, from: usize) {
		let mut buf = [0u8; FRAME_LEN];
		loop {
			let mut lost = Vec::new();
			while let Some((destination, len)) = self.nodes[from].poll_transmit(&mut buf) {
				self.sent += 1;
				let dropped = self
					.loss_every
					.is_some_and(|every| self.sent.is_multiple_of(every));
				let arrival = self.now_micros() + LATENCY_MICROS;
				let mut arrived = false;
				for to in 0..self.nodes.len() {
					let addressed = match destination {
						Destination::Broadcast => true,
						Destination::Unicast(mac) => mac == self.macs[to],
					};
					if addressed && self.in_range[from][to] && !dropped {
						let key = (arrival, self.sent * 64 + to as u64);
						self.air
							.insert(key, (to, self.macs[from], buf[..len].to_vec()));
						arrived = true;
					}
				}
				// ESP-NOW reports unicast frames that were not acknowledged.
				if let Destination::Unicast(mac) = destination
					&& !arrived
				{
					lost.push((mac, buf[..len].to_vec()));
				}
			}
			if lost.is_empty() {
				return;
			}
			for (mac, frame) in lost {
				self.nodes[from].frame_lost(mac, &frame);
			}
		}
	}

	fn deliver_due(&mut self) {
		let now = self.now_micros();
		while let Some(entry) = self.air.first_entry() {
			if entry.key().0 > now {
				break;
			}
			let (to, from, frame) = entry.remove();
			self.nodes[to].handle_frame(from, &frame);
			self.transmit(to);
		}
	}

	/// Delivers due frames, runs expired timers and sends what results,
	/// without moving the clock.
	pub fn step(&mut self) {
		self.deliver_due();
		for index in 0..self.nodes.len() {
			let now = self.nodes[index].now();
			if self.nodes[index].poll_timeout().is_some_and(|at| at <= now) {
				self.nodes[index].handle_timeout();
			}
			self.transmit(index);
		}
	}

	pub fn set_clock(&mut self, micros: u64) {
		self.clock_micros.store(micros, Ordering::Relaxed);
	}

	/// Advances the simulation by `duration`, one millisecond at a time.
	pub fn run(&mut self, duration: Duration) {
		let end = self.now_micros() + duration.as_micros() as u64;
		while self.now_micros() < end {
			self.step();
			self.clock_micros.fetch_add(1_000, Ordering::Relaxed);
		}
	}

	/// Runs until `done` returns true, or `limit` passes.
	pub fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&mut Self) -> bool) -> bool {
		let end = self.now_micros() + limit.as_micros() as u64;
		while self.now_micros() < end {
			if done(self) {
				return true;
			}
			self.run(Duration::from_millis(10));
		}
		done(self)
	}

	/// Pings `to` from `from` once a second until a reply arrives.
	pub fn ping(&mut self, from: usize, to: usize, limit: Duration) -> bool {
		let source = Ipv6Addr::from(self.nodes[from].address().0);
		let target = Ipv6Addr::from(self.nodes[to].address().0);
		let request = echo_request(target, source, b"ping");
		let mut last_sent: Option<u64> = None;
		self.run_until(limit, |radio| {
			if last_sent.is_none_or(|sent| radio.now_micros() >= sent + 1_000_000) {
				let _ = radio.nodes[from].send_ipv6(&request);
				last_sent = Some(radio.now_micros());
			}
			while let Some(packet) = radio.nodes[from].recv_ipv6() {
				if is_echo_reply(&packet, target, b"ping") {
					return true;
				}
			}
			false
		})
	}

	/// Whether every node has finished the Yggdrasil handshake with each
	/// neighbor in range.
	pub fn all_peered(&self) -> bool {
		(0..self.nodes.len()).all(|a| {
			let peered: Vec<Mac> = self.nodes[a]
				.neighbors()
				.filter(|(_, key)| key.is_some())
				.map(|(mac, _)| mac)
				.collect();
			(0..self.nodes.len())
				.filter(|&b| self.in_range[a][b])
				.all(|b| peered.contains(&self.macs[b]))
		})
	}
}

pub fn echo_request(dst: Ipv6Addr, src: Ipv6Addr, data: &[u8]) -> Vec<u8> {
	let mut body = vec![0x12, 0x34, 0, 1];
	body.extend_from_slice(data);
	create_icmpv6(dst, src, 128, 0, &body)
}

pub fn is_echo_reply(packet: &[u8], from: Ipv6Addr, data: &[u8]) -> bool {
	packet.len() >= 48
		&& packet[6] == 58
		&& packet[8..24] == from.octets()
		&& packet[40] == 129
		&& packet[48..] == *data
}
