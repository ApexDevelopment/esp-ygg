//! The hotspot: a WiFi network of the board's own that phones and laptops
//! join to reach Yggdrasil without installing anything. Router
//! advertisements give each device an address in the board's `300:` subnet
//! and a route to the rest of `200::/7`, and the board forwards their
//! packets between the WiFi network and the mesh.
//!
//! This handles Ethernet frames from the access point; the firmware moves
//! them to and from the radio.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::time::Duration;

use yggdrasil_espnow::Instant;

use crate::device::MTU;

pub type Mac = [u8; 6];

const ETHERNET_HEADER_LEN: usize = 14;
const ETHERTYPE_IPV6: [u8; 2] = [0x86, 0xdd];
const IPV6_HEADER_LEN: usize = 40;
const ICMPV6: u8 = 58;
const UDP: u8 = 17;

const ROUTER_SOLICITATION: u8 = 133;
const ROUTER_ADVERTISEMENT: u8 = 134;
const NEIGHBOR_SOLICITATION: u8 = 135;
const NEIGHBOR_ADVERTISEMENT: u8 = 136;
const DESTINATION_UNREACHABLE: u8 = 1;
const PORT_UNREACHABLE: u8 = 4;

const ALL_NODES: [u8; 16] = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

/// Devices are told about the network again this often, besides when they ask.
const ADVERT_INTERVAL: Duration = Duration::from_secs(100);
const ROUTER_LIFETIME_SECS: u16 = 1800;
const PREFIX_VALID_SECS: u32 = 24 * 60 * 60;
const PREFIX_PREFERRED_SECS: u32 = 4 * 60 * 60;
/// How many devices the board remembers the hardware addresses of.
const MAX_NEIGHBORS: usize = 16;
const NEIGHBOR_LIFETIME: Duration = Duration::from_secs(10 * 60);

pub struct Hotspot {
	mac: Mac,
	link_local: [u8; 16],
	subnet: [u8; 8],
	neighbors: BTreeMap<[u8; 16], (Mac, Instant)>,
	/// A packet for each device whose hardware address is being asked for.
	waiting: BTreeMap<[u8; 16], Vec<u8>>,
	frames: VecDeque<Vec<u8>>,
	next_advert: Instant,
}

impl Hotspot {
	/// `subnet` is the first half of the board's `300:` subnet.
	pub fn new(mac: Mac, subnet: [u8; 8], now: Instant) -> Self {
		Hotspot {
			mac,
			link_local: link_local(mac),
			subnet,
			neighbors: BTreeMap::new(),
			waiting: BTreeMap::new(),
			frames: VecDeque::new(),
			next_advert: now,
		}
	}

	pub fn in_subnet(&self, address: &[u8; 16]) -> bool {
		address[..8] == self.subnet
	}

	/// How many devices have sent something recently.
	pub fn devices(&self) -> usize {
		let mut macs: Vec<Mac> = self.neighbors.values().map(|(mac, _)| *mac).collect();
		macs.sort_unstable();
		macs.dedup();
		macs.len()
	}

	/// Takes a frame from the WiFi network. Returns the IPv6 packet inside
	/// if it is for the mesh.
	pub fn receive(&mut self, frame: &[u8], now: Instant) -> Option<Vec<u8>> {
		let (header, packet) = frame.split_at_checked(ETHERNET_HEADER_LEN)?;
		if header[12..14] != ETHERTYPE_IPV6 {
			return None;
		}
		let to_mac: Mac = header[..6].try_into().ok()?;
		let from_mac: Mac = header[6..12].try_into().ok()?;
		let packet = ipv6_packet(packet)?;
		let (source, destination) = (address_at(packet, 8), address_at(packet, 24));
		if self.in_subnet(&source) {
			self.learn(source, from_mac, now);
		}
		if packet[6] == ICMPV6
			&& (ROUTER_SOLICITATION..=NEIGHBOR_ADVERTISEMENT).contains(&packet[40])
		{
			self.neighbor_discovery(packet, from_mac, now);
			return None;
		}
		if to_mac != self.mac {
			return None;
		}
		if destination == self.link_local {
			self.refuse(packet, from_mac);
			return None;
		}
		let is_yggdrasil = destination[0] & 0xfe == 0x02;
		if !is_yggdrasil || !self.in_subnet(&source) || packet[7] <= 1 {
			return None;
		}
		let mut packet = packet.to_vec();
		packet[7] -= 1;
		if self.in_subnet(&destination) {
			self.send(packet);
			return None;
		}
		Some(packet)
	}

	/// Sends an IPv6 packet to the device in the subnet it is addressed to.
	pub fn send(&mut self, packet: Vec<u8>) {
		if ipv6_packet(&packet).is_none() {
			return;
		}
		let destination = address_at(&packet, 24);
		if let Some((mac, _)) = self.neighbors.get(&destination) {
			let frame = ethernet(*mac, self.mac, &packet);
			self.frames.push_back(frame);
		} else if self.waiting.len() < MAX_NEIGHBORS || self.waiting.contains_key(&destination) {
			self.waiting.insert(destination, packet);
			self.solicit(destination);
		}
	}

	/// The next frame for the WiFi network.
	pub fn poll_frame(&mut self) -> Option<Vec<u8>> {
		self.frames.pop_front()
	}

	pub fn poll_at(&self) -> Instant {
		self.next_advert
	}

	/// Advertises the network when due, and forgets devices gone quiet.
	pub fn handle_timeout(&mut self, now: Instant) {
		if now >= self.next_advert {
			self.advertise(multicast_mac(&ALL_NODES), ALL_NODES);
			self.next_advert = now + ADVERT_INTERVAL;
		}
		self.neighbors
			.retain(|_, (_, seen)| now.saturating_duration_since(*seen) < NEIGHBOR_LIFETIME);
	}

	fn learn(&mut self, address: [u8; 16], mac: Mac, now: Instant) {
		if !self.neighbors.contains_key(&address) && self.neighbors.len() >= MAX_NEIGHBORS {
			let oldest = self
				.neighbors
				.iter()
				.min_by_key(|(_, (_, seen))| *seen)
				.map(|(address, _)| *address);
			if let Some(oldest) = oldest {
				self.neighbors.remove(&oldest);
			}
		}
		self.neighbors.insert(address, (mac, now));
		if let Some(packet) = self.waiting.remove(&address) {
			self.frames.push_back(ethernet(mac, self.mac, &packet));
		}
	}

	fn neighbor_discovery(&mut self, packet: &[u8], from_mac: Mac, now: Instant) {
		// Neighbor discovery from off the network would have a lower hop limit.
		if packet[7] != 255 {
			return;
		}
		let source = address_at(packet, 8);
		let message = &packet[IPV6_HEADER_LEN..];
		match message[0] {
			ROUTER_SOLICITATION => {
				self.advertise(multicast_mac(&ALL_NODES), ALL_NODES);
			}
			NEIGHBOR_SOLICITATION if message.len() >= 24 => {
				let target = address_at(message, 8);
				if target == self.link_local {
					let (mac, to) = if source == [0; 16] {
						(multicast_mac(&ALL_NODES), ALL_NODES)
					} else {
						(from_mac, source)
					};
					self.advertise_neighbor(mac, to);
				}
			}
			NEIGHBOR_ADVERTISEMENT if message.len() >= 24 => {
				let target = address_at(message, 8);
				if self.in_subnet(&target) {
					let mac = link_layer_option(&message[24..], 2).unwrap_or(from_mac);
					self.learn(target, mac, now);
				}
			}
			_ => {}
		}
	}

	/// Tells devices the board is their router, which subnet to take an
	/// address from, and that all of `200::/7` is reached through it.
	fn advertise(&mut self, mac: Mac, to: [u8; 16]) {
		let mut message = Vec::with_capacity(112);
		message.extend_from_slice(&[ROUTER_ADVERTISEMENT, 0, 0, 0, 64]);
		// A low preference, so a default route through another network wins.
		message.push(0b11 << 3);
		message.extend_from_slice(&ROUTER_LIFETIME_SECS.to_be_bytes());
		message.extend_from_slice(&[0; 8]);
		// The board's hardware address
		message.extend_from_slice(&[1, 1]);
		message.extend_from_slice(&self.mac);
		message.extend_from_slice(&[5, 1, 0, 0]);
		message.extend_from_slice(&(MTU as u32).to_be_bytes());
		// The subnet, for devices to make their addresses in
		message.extend_from_slice(&[3, 4, 64, 0b1100_0000]);
		message.extend_from_slice(&PREFIX_VALID_SECS.to_be_bytes());
		message.extend_from_slice(&PREFIX_PREFERRED_SECS.to_be_bytes());
		message.extend_from_slice(&[0; 4]);
		message.extend_from_slice(&self.subnet);
		message.extend_from_slice(&[0; 8]);
		// A route to 200::/7 with high preference
		message.extend_from_slice(&[24, 2, 7, 0b01 << 3]);
		message.extend_from_slice(&u32::from(ROUTER_LIFETIME_SECS).to_be_bytes());
		message.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0]);
		// Phones only stay on a network that names a DNS server. The board
		// runs none, but refuses queries at once so lookups fail fast.
		message.extend_from_slice(&[25, 3, 0, 0]);
		message.extend_from_slice(&u32::from(ROUTER_LIFETIME_SECS).to_be_bytes());
		message.extend_from_slice(&self.link_local);
		self.send_icmp(mac, to, message);
	}

	fn advertise_neighbor(&mut self, mac: Mac, to: [u8; 16]) {
		// Router, solicited and override
		let flags = if to == ALL_NODES {
			0b1010_0000
		} else {
			0b1110_0000
		};
		let mut message = Vec::from([NEIGHBOR_ADVERTISEMENT, 0, 0, 0, flags, 0, 0, 0]);
		message.extend_from_slice(&self.link_local);
		message.extend_from_slice(&[2, 1]);
		message.extend_from_slice(&self.mac);
		self.send_icmp(mac, to, message);
	}

	/// Asks the device with `target` for its hardware address.
	fn solicit(&mut self, target: [u8; 16]) {
		let mut group = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff, 0, 0, 0];
		group[13..].copy_from_slice(&target[13..]);
		let mut message = Vec::from([NEIGHBOR_SOLICITATION, 0, 0, 0, 0, 0, 0, 0]);
		message.extend_from_slice(&target);
		message.extend_from_slice(&[1, 1]);
		message.extend_from_slice(&self.mac);
		self.send_icmp(multicast_mac(&group), group, message);
	}

	/// Answers a UDP packet sent to the board's link-local address, such as
	/// a DNS query, with "port unreachable".
	fn refuse(&mut self, packet: &[u8], from_mac: Mac) {
		if packet[6] != UDP {
			return;
		}
		let quoted = packet.len().min(MTU - IPV6_HEADER_LEN - 8);
		let mut message = Vec::from([DESTINATION_UNREACHABLE, PORT_UNREACHABLE, 0, 0, 0, 0, 0, 0]);
		message.extend_from_slice(&packet[..quoted]);
		self.send_icmp(from_mac, address_at(packet, 8), message);
	}

	fn send_icmp(&mut self, mac: Mac, to: [u8; 16], mut message: Vec<u8>) {
		let checksum = icmp_checksum(&self.link_local, &to, &message);
		message[2..4].copy_from_slice(&checksum.to_be_bytes());
		let mut packet = Vec::with_capacity(IPV6_HEADER_LEN + message.len());
		packet.extend_from_slice(&[0x60, 0, 0, 0]);
		packet.extend_from_slice(&(message.len() as u16).to_be_bytes());
		packet.extend_from_slice(&[ICMPV6, 255]);
		packet.extend_from_slice(&self.link_local);
		packet.extend_from_slice(&to);
		packet.extend_from_slice(&message);
		self.frames.push_back(ethernet(mac, self.mac, &packet));
	}
}

/// The packet trimmed to the length its header gives, if it is IPv6.
fn ipv6_packet(packet: &[u8]) -> Option<&[u8]> {
	if packet.len() < IPV6_HEADER_LEN || packet[0] >> 4 != 6 {
		return None;
	}
	let payload = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
	let packet = packet.get(..IPV6_HEADER_LEN + payload)?;
	// Neighbor discovery messages need at least their fixed part.
	if packet[6] == ICMPV6 && payload < 8 {
		return None;
	}
	Some(packet)
}

fn address_at(bytes: &[u8], offset: usize) -> [u8; 16] {
	bytes[offset..offset + 16].try_into().expect("16 bytes")
}

/// The hardware address in a neighbor discovery option of `kind`.
fn link_layer_option(mut options: &[u8], kind: u8) -> Option<Mac> {
	while let [option_kind, length, ..] = *options {
		let length = usize::from(length) * 8;
		if length == 0 || options.len() < length {
			return None;
		}
		if option_kind == kind && length >= 8 {
			return options[2..8].try_into().ok();
		}
		options = &options[length..];
	}
	None
}

fn link_local(mac: Mac) -> [u8; 16] {
	let mut address = [0u8; 16];
	address[..2].copy_from_slice(&[0xfe, 0x80]);
	address[8..].copy_from_slice(&[
		mac[0] ^ 2,
		mac[1],
		mac[2],
		0xff,
		0xfe,
		mac[3],
		mac[4],
		mac[5],
	]);
	address
}

fn multicast_mac(group: &[u8; 16]) -> Mac {
	[0x33, 0x33, group[12], group[13], group[14], group[15]]
}

fn ethernet(to: Mac, from: Mac, packet: &[u8]) -> Vec<u8> {
	let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + packet.len());
	frame.extend_from_slice(&to);
	frame.extend_from_slice(&from);
	frame.extend_from_slice(&ETHERTYPE_IPV6);
	frame.extend_from_slice(packet);
	frame
}

fn icmp_checksum(source: &[u8; 16], destination: &[u8; 16], message: &[u8]) -> u16 {
	let mut sum: u32 = 0;
	let mut add = |bytes: &[u8]| {
		for pair in bytes.chunks(2) {
			sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
		}
	};
	add(source);
	add(destination);
	add(&(message.len() as u32).to_be_bytes());
	add(&[0, 0, 0, ICMPV6]);
	add(message);
	while sum > 0xffff {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	!(sum as u16)
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	const BOARD: Mac = [0x24, 0, 0, 0, 0, 1];
	const PHONE: Mac = [0x8a, 0, 0, 0, 0, 2];
	const SUBNET: [u8; 8] = [0x03, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde];

	fn at(seconds: u64) -> Instant {
		Instant::from_duration(Duration::from_secs(seconds))
	}

	fn phone_address() -> [u8; 16] {
		let mut address = [0u8; 16];
		address[..8].copy_from_slice(&SUBNET);
		address[15] = 7;
		address
	}

	fn remote_address() -> [u8; 16] {
		let mut address = [0u8; 16];
		address[..2].copy_from_slice(&[0x02, 0x01]);
		address[15] = 9;
		address
	}

	fn packet(
		source: [u8; 16],
		destination: [u8; 16],
		next: u8,
		hops: u8,
		payload: &[u8],
	) -> Vec<u8> {
		let mut packet = vec![0x60, 0, 0, 0];
		packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
		packet.extend_from_slice(&[next, hops]);
		packet.extend_from_slice(&source);
		packet.extend_from_slice(&destination);
		packet.extend_from_slice(payload);
		packet
	}

	fn icmp(source: [u8; 16], destination: [u8; 16], mut message: Vec<u8>) -> Vec<u8> {
		let checksum = icmp_checksum(&source, &destination, &message);
		message[2..4].copy_from_slice(&checksum.to_be_bytes());
		packet(source, destination, ICMPV6, 255, &message)
	}

	fn frames(hotspot: &mut Hotspot) -> Vec<Vec<u8>> {
		core::iter::from_fn(|| hotspot.poll_frame()).collect()
	}

	/// The ICMPv6 message in a frame, after checking its checksum.
	fn icmp_message(frame: &[u8]) -> &[u8] {
		let packet = &frame[ETHERNET_HEADER_LEN..];
		assert_eq!(packet[6], ICMPV6);
		let message = &packet[IPV6_HEADER_LEN..];
		let (source, destination) = (address_at(packet, 8), address_at(packet, 24));
		assert_eq!(icmp_checksum(&source, &destination, message), 0);
		message
	}

	#[test]
	fn advertises_the_subnet_when_asked() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		hotspot.handle_timeout(at(0));
		frames(&mut hotspot);
		let phone_link_local = link_local(PHONE);
		let solicitation = icmp(
			phone_link_local,
			[0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
			vec![ROUTER_SOLICITATION, 0, 0, 0, 0, 0, 0, 0],
		);
		assert_eq!(
			hotspot.receive(
				&ethernet([0x33, 0x33, 0, 0, 0, 2], PHONE, &solicitation),
				at(1)
			),
			None
		);
		let sent = frames(&mut hotspot);
		assert_eq!(sent.len(), 1);
		let advert = icmp_message(&sent[0]);
		assert_eq!(advert[0], ROUTER_ADVERTISEMENT);
		let prefix = advert
			.windows(2)
			.position(|pair| pair == [3, 4])
			.expect("a prefix option");
		assert_eq!(advert[prefix + 16..prefix + 24], SUBNET);
	}

	#[test]
	fn answers_neighbor_solicitations_for_itself() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		let mut message = vec![NEIGHBOR_SOLICITATION, 0, 0, 0, 0, 0, 0, 0];
		message.extend_from_slice(&link_local(BOARD));
		message.extend_from_slice(&[1, 1]);
		message.extend_from_slice(&PHONE);
		let solicitation = icmp(phone_address(), link_local(BOARD), message);
		hotspot.receive(&ethernet(BOARD, PHONE, &solicitation), at(1));
		let sent = frames(&mut hotspot);
		assert_eq!(sent[0][..6], PHONE);
		let advert = icmp_message(&sent[0]);
		assert_eq!(advert[0], NEIGHBOR_ADVERTISEMENT);
		assert_eq!(link_layer_option(&advert[24..], 2), Some(BOARD));
	}

	#[test]
	fn forwards_between_devices_and_the_mesh() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		let outgoing = packet(phone_address(), remote_address(), UDP, 64, b"hello");
		let forwarded = hotspot
			.receive(&ethernet(BOARD, PHONE, &outgoing), at(1))
			.expect("forwarded to the mesh");
		assert_eq!(forwarded[7], 63);
		assert_eq!(forwarded[8..], outgoing[8..]);

		let reply = packet(remote_address(), phone_address(), UDP, 64, b"hi");
		hotspot.send(reply.clone());
		let sent = frames(&mut hotspot);
		assert_eq!(sent, vec![ethernet(PHONE, BOARD, &reply)]);

		// Phones use a second, temporary address too.
		let mut temporary = phone_address();
		temporary[15] = 8;
		let outgoing = packet(temporary, remote_address(), UDP, 64, b"again");
		hotspot.receive(&ethernet(BOARD, PHONE, &outgoing), at(2));
		assert_eq!(hotspot.devices(), 1);
	}

	#[test]
	fn asks_for_unknown_devices_and_sends_once_they_answer() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		let reply = packet(remote_address(), phone_address(), UDP, 64, b"hi");
		hotspot.send(reply.clone());
		let sent = frames(&mut hotspot);
		assert_eq!(icmp_message(&sent[0])[0], NEIGHBOR_SOLICITATION);
		assert_eq!(sent[0][..3], [0x33, 0x33, 0xff]);

		let mut message = vec![NEIGHBOR_ADVERTISEMENT, 0, 0, 0, 0b0110_0000, 0, 0, 0];
		message.extend_from_slice(&phone_address());
		message.extend_from_slice(&[2, 1]);
		message.extend_from_slice(&PHONE);
		let advert = icmp(phone_address(), link_local(BOARD), message);
		hotspot.receive(&ethernet(BOARD, PHONE, &advert), at(1));
		assert_eq!(frames(&mut hotspot), vec![ethernet(PHONE, BOARD, &reply)]);
	}

	#[test]
	fn drops_what_it_cannot_route() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		let internet = [0x26, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
		let to_internet = packet(phone_address(), internet, UDP, 64, b"x");
		assert_eq!(
			hotspot.receive(&ethernet(BOARD, PHONE, &to_internet), at(1)),
			None
		);
		let expiring = packet(phone_address(), remote_address(), UDP, 1, b"x");
		assert_eq!(
			hotspot.receive(&ethernet(BOARD, PHONE, &expiring), at(1)),
			None
		);
		let spoofed = packet(remote_address(), remote_address(), UDP, 64, b"x");
		assert_eq!(
			hotspot.receive(&ethernet(BOARD, PHONE, &spoofed), at(1)),
			None
		);
	}

	#[test]
	fn refuses_dns_queries() {
		let mut hotspot = Hotspot::new(BOARD, SUBNET, at(0));
		let query = packet(phone_address(), link_local(BOARD), UDP, 64, &[0; 20]);
		hotspot.receive(&ethernet(BOARD, PHONE, &query), at(1));
		let sent = frames(&mut hotspot);
		assert_eq!(
			icmp_message(&sent[0])[..2],
			[DESTINATION_UNREACHABLE, PORT_UNREACHABLE]
		);
	}
}
