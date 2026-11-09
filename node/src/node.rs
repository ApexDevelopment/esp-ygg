use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv6Addr;
use core::num::NonZeroU32;
use core::time::Duration;

use espnow_reliable::Event as LinkEvent;
use petrified::encrypted::PacketConn;
use petrified::network;
use petrified::types::{Error, Event, Instant, PacketConn as _, Platform, PrivateKey, PublicKey};
use yggdrasil_proto::address::{Address, Subnet, subnet_for_key};
use yggdrasil_proto::handshake::{HandshakeError, Metadata};
use yggdrasil_proto::icmpv6::{IPV6_HEADER_LEN, create_icmpv6};
use yggdrasil_proto::ipv6::{Incoming, KeyStore, Outgoing, WriteError};
use yggdrasil_proto::requests::{self, NodeView};
use yggdrasil_proto::session::{TYPE_SESSION_PROTO, TYPE_SESSION_TRAFFIC};

use crate::espnow::{Destination, Frame, Mac, PeerLink, encode_beacon, encode_link_frame};
use crate::stream::{Stream, StreamError};

/// How long a neighbor has to finish the reliable link handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a peer has to finish the Yggdrasil handshake once the link is up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to ignore a neighbor whose handshake failed.
const COOLDOWN: Duration = Duration::from_secs(30);

const NEXT_HEADER_ICMPV6: u8 = 58;
const ICMPV6_ECHO_REQUEST: u8 = 128;
const ICMPV6_ECHO_REPLY: u8 = 129;

pub struct Config {
	pub private_key: PrivateKey,
	/// The link password. Peers must use the same one.
	pub password: Vec<u8>,
	/// This node's nodeinfo JSON, as from
	/// [`yggdrasil_proto::requests::build_nodeinfo`].
	pub nodeinfo: Vec<u8>,
	/// The most ESP-NOW neighbors to peer with at once.
	pub max_neighbors: usize,
	pub beacon_interval: Duration,
	/// The largest routing message accepted from a peer. A peer that sends a
	/// larger one is disconnected.
	pub max_message_size: u64,
	/// The largest IPv6 packet this node accepts.
	pub mtu: u64,
	/// How many bytes of IPv6 packets to hold for [`Node::recv_ipv6`] before
	/// dropping the oldest.
	pub max_inbox_bytes: usize,
	/// Timing for the reliable link to each neighbor.
	pub link: espnow_reliable::Config,
}

impl Config {
	pub fn new(private_key: PrivateKey) -> Self {
		Config {
			private_key,
			password: Vec::new(),
			nodeinfo: b"{}".to_vec(),
			max_neighbors: 6,
			beacon_interval: Duration::from_secs(2),
			max_message_size: 16 * 1024,
			mtu: 1280,
			max_inbox_bytes: 8 * 1024,
			// ESP-NOW already retries each frame in hardware, so resends can
			// come quickly without much risk of crowding the radio.
			link: espnow_reliable::Config {
				max_retransmit_timeout: Duration::from_millis(250),
				max_retransmits: 20,
				..espnow_reliable::Config::default()
			},
		}
	}
}

#[derive(Debug)]
pub enum NewError {
	/// The private key does not make a valid public key.
	Key(Error),
	/// The password is too long.
	Password(HandshakeError),
}

impl core::fmt::Display for NewError {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		match self {
			NewError::Key(error) => write!(f, "invalid private key: {error:?}"),
			NewError::Password(error) => write!(f, "invalid password: {error}"),
		}
	}
}

/// A stream peer, such as a TCP connection, opened with [`Node::open_stream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(u32);

struct Neighbor {
	link: Box<PeerLink>,
	/// Present while the link is connected.
	stream: Option<Stream>,
	/// When the link was created or last connected.
	since: Instant,
}

struct StreamEntry {
	stream: Stream,
	opened: Instant,
}

pub struct Node<P> {
	conn: PacketConn,
	platform: P,
	password: Vec<u8>,
	handshake: Vec<u8>,
	nodeinfo: Vec<u8>,
	keys: KeyStore,
	mtu: u64,
	inbox: VecDeque<Vec<u8>>,
	inbox_bytes: usize,
	max_inbox_bytes: usize,
	max_neighbors: usize,
	link_config: espnow_reliable::Config,
	beacon_interval: Duration,
	beacon_at: Instant,
	neighbors: BTreeMap<Mac, Neighbor>,
	cooldown: BTreeMap<Mac, Instant>,
	streams: BTreeMap<StreamId, StreamEntry>,
	next_stream: u32,
}

impl<P: Platform + Clone + Send + 'static> Node<P> {
	pub fn new(config: Config, platform: P) -> Result<Self, NewError> {
		let net_config = network::Config::default()
			.with_bloom_transform(|key| subnet_for_key(key).get_key())
			.with_peer_max_message_size(config.max_message_size)
			.with_path_notify_queue();
		let conn = PacketConn::new(&config.private_key, platform.clone(), net_config)
			.map_err(NewError::Key)?;
		let mut metadata = Metadata::base();
		metadata.public_key = Some(conn.public_key());
		let handshake = metadata
			.encode(&config.private_key, &config.password)
			.map_err(NewError::Password)?;
		let keys = KeyStore::new(&conn.public_key());
		let beacon_at = platform.now();
		Ok(Node {
			conn,
			platform,
			password: config.password,
			handshake,
			nodeinfo: config.nodeinfo,
			keys,
			mtu: config.mtu,
			inbox: VecDeque::new(),
			inbox_bytes: 0,
			max_inbox_bytes: config.max_inbox_bytes,
			max_neighbors: config.max_neighbors,
			link_config: config.link,
			beacon_interval: config.beacon_interval,
			beacon_at,
			neighbors: BTreeMap::new(),
			cooldown: BTreeMap::new(),
			streams: BTreeMap::new(),
			next_stream: 0,
		})
	}
}

impl<P: Platform> Node<P> {
	pub fn public_key(&self) -> PublicKey {
		self.conn.public_key()
	}

	pub fn address(&self) -> Address {
		self.keys.address()
	}

	pub fn subnet(&self) -> Subnet {
		self.keys.subnet()
	}

	pub fn now(&self) -> Instant {
		self.platform.now()
	}

	/// The routing layer, for its debug information.
	pub fn network(&self) -> &network::PacketConn {
		self.conn.network()
	}

	/// ESP-NOW neighbors, with their keys once the Yggdrasil handshake is done.
	pub fn neighbors(&self) -> impl Iterator<Item = (Mac, Option<PublicKey>)> + '_ {
		self.neighbors.iter().map(|(mac, neighbor)| {
			let key = neighbor.stream.as_ref().and_then(Stream::key);
			(*mac, key)
		})
	}

	/// Handles a frame received over ESP-NOW from `from`.
	pub fn handle_frame(&mut self, from: Mac, frame: &[u8]) {
		let now = self.now();
		match Frame::decode(frame) {
			Some(Frame::Beacon(key)) if key != self.public_key() => {
				self.ensure_neighbor(from, now);
			}
			Some(Frame::Link(bytes)) if self.ensure_neighbor(from, now) => {
				let neighbor = self.neighbors.get_mut(&from).expect("ensured");
				let mut messages = Vec::new();
				neighbor.link.handle(bytes, link_time(now), |message| {
					messages.push(message.to_vec())
				});
				self.service_neighbor(from, &messages, now);
			}
			_ => {}
		}
		self.process();
	}

	/// Writes the next frame to send into `buf`, which must hold
	/// [`crate::FRAME_LEN`] bytes. Call this until it returns `None` after
	/// every other call to the node.
	pub fn poll_transmit(&mut self, buf: &mut [u8]) -> Option<(Destination, usize)> {
		let now = self.now();
		if now >= self.beacon_at {
			self.beacon_at = now + self.beacon_interval;
			return Some((
				Destination::Broadcast,
				encode_beacon(&self.public_key(), buf),
			));
		}
		let mut failed = Vec::new();
		let mut frame = None;
		for (mac, neighbor) in &mut self.neighbors {
			if let Some(len) = encode_link_frame(&mut neighbor.link, link_time(now), buf) {
				frame = Some((Destination::Unicast(*mac), len));
				break;
			}
			if neighbor.link.is_failed() {
				failed.push(*mac);
			}
		}
		for mac in failed {
			log::info!("ESP-NOW link to {} failed", MacDisplay(&mac));
			self.drop_neighbor(mac, None);
		}
		frame
	}

	/// When [`Node::handle_timeout`] should next be called. The time may have
	/// passed already.
	pub fn poll_timeout(&self) -> Option<Instant> {
		let neighbors = self.neighbors.values().flat_map(|neighbor| {
			let link = neighbor.link.poll_timeout().map(from_link_time);
			let deadline = match &neighbor.stream {
				None => neighbor.since + CONNECT_TIMEOUT,
				Some(stream) if stream.is_handshaking() => neighbor.since + HANDSHAKE_TIMEOUT,
				Some(_) => Instant::from_duration(Duration::MAX),
			};
			link.into_iter().chain([deadline])
		});
		let streams = self
			.streams
			.values()
			.filter(|entry| entry.stream.is_handshaking())
			.map(|entry| entry.opened + HANDSHAKE_TIMEOUT);
		[self.conn.poll_timeout(), Some(self.beacon_at)]
			.into_iter()
			.flatten()
			.chain(neighbors)
			.chain(streams)
			.min()
	}

	/// Runs expired timers. Call [`Node::poll_transmit`] afterwards.
	pub fn handle_timeout(&mut self) {
		let now = self.now();
		if self
			.conn
			.poll_timeout()
			.is_some_and(|deadline| deadline <= now)
		{
			self.conn.handle_timeout();
		}
		let stale: Vec<(Mac, bool)> = self
			.neighbors
			.iter()
			.filter_map(|(mac, neighbor)| match &neighbor.stream {
				None if now >= neighbor.since + CONNECT_TIMEOUT => Some((*mac, false)),
				Some(stream)
					if stream.is_handshaking() && now >= neighbor.since + HANDSHAKE_TIMEOUT =>
				{
					Some((*mac, true))
				}
				_ => None,
			})
			.collect();
		for (mac, handshake) in stale {
			if handshake {
				log::warn!("Yggdrasil handshake with {} timed out", MacDisplay(&mac));
			}
			self.drop_neighbor(mac, handshake.then_some(now + COOLDOWN));
		}
		for entry in self.streams.values_mut() {
			if entry.stream.is_handshaking() && now >= entry.opened + HANDSHAKE_TIMEOUT {
				entry.stream.close(&mut self.conn);
			}
		}
		self.cooldown.retain(|_, until| *until > now);
		self.process();
	}

	/// Starts a peering over a reliable byte stream, such as a TCP connection.
	pub fn open_stream(&mut self) -> StreamId {
		let id = StreamId(self.next_stream);
		self.next_stream = self.next_stream.wrapping_add(1);
		let entry = StreamEntry {
			stream: Stream::new(self.handshake.clone()),
			opened: self.now(),
		};
		self.streams.insert(id, entry);
		id
	}

	/// Handles bytes read from a stream. On error, close the stream.
	pub fn stream_receive(&mut self, id: StreamId, data: &[u8]) -> Result<(), StreamError> {
		let Some(entry) = self.streams.get_mut(&id) else {
			return Err(StreamError::Protocol(Error::Closed));
		};
		let result = entry.stream.receive(&mut self.conn, &self.password, data);
		if let Err(error) = &result {
			log::warn!("Stream peer failed: {error}");
		}
		self.process();
		result
	}

	/// Bytes waiting to be written to a stream.
	pub fn stream_transmit(&self, id: StreamId) -> &[u8] {
		match self.streams.get(&id) {
			Some(entry) => entry.stream.transmit(&self.conn),
			None => &[],
		}
	}

	/// Marks the first `n` bytes of [`Node::stream_transmit`] as written.
	pub fn stream_consume(&mut self, id: StreamId, n: usize) {
		if let Some(entry) = self.streams.get_mut(&id) {
			entry.stream.consume(&mut self.conn, n);
		}
	}

	/// The key of a stream's peer, once the handshake is done.
	pub fn stream_key(&self, id: StreamId) -> Option<PublicKey> {
		self.streams.get(&id)?.stream.key()
	}

	/// Whether a stream is still usable. Once false, close it with
	/// [`Node::close_stream`].
	pub fn is_stream_open(&self, id: StreamId) -> bool {
		self.streams
			.get(&id)
			.is_some_and(|entry| !entry.stream.is_closed())
	}

	pub fn close_stream(&mut self, id: StreamId) {
		if let Some(mut entry) = self.streams.remove(&id) {
			entry.stream.close(&mut self.conn);
		}
		self.process();
	}

	/// Sends an IPv6 packet from this node. If the destination's key is not
	/// known yet, the packet waits while it is looked up.
	pub fn send_ipv6(&mut self, packet: &[u8]) -> Result<(), WriteError> {
		let result = self.route_ipv6(packet);
		self.process();
		result
	}

	/// Takes the next IPv6 packet addressed to this node that the node did
	/// not answer itself.
	pub fn recv_ipv6(&mut self) -> Option<Vec<u8>> {
		let packet = self.inbox.pop_front()?;
		self.inbox_bytes -= packet.len();
		Some(packet)
	}

	fn route_ipv6(&mut self, packet: &[u8]) -> Result<(), WriteError> {
		match self.keys.outgoing(packet, self.now())? {
			Outgoing::Send(key) => send_traffic(&mut self.conn, &key, packet),
			Outgoing::Lookup(partial) => self.conn.send_lookup(&partial),
		}
		Ok(())
	}

	/// Creates a link for a new neighbor if there is room. Returns whether
	/// one exists afterwards.
	fn ensure_neighbor(&mut self, mac: Mac, now: Instant) -> bool {
		if self.neighbors.contains_key(&mac) {
			return true;
		}
		if self.cooldown.get(&mac).is_some_and(|until| *until > now)
			|| self.neighbors.len() >= self.max_neighbors
		{
			return false;
		}
		let session = self.random_session();
		let link = Box::new(PeerLink::new(session, self.link_config));
		self.neighbors.insert(
			mac,
			Neighbor {
				link,
				stream: None,
				since: now,
			},
		);
		true
	}

	fn random_session(&mut self) -> NonZeroU32 {
		loop {
			let mut bytes = [0u8; 4];
			self.platform.fill_random(&mut bytes);
			if let Some(session) = NonZeroU32::new(u32::from_le_bytes(bytes)) {
				return session;
			}
		}
	}

	/// Acts on a neighbor's link events and the messages it just delivered.
	fn service_neighbor(&mut self, mac: Mac, messages: &[Vec<u8>], now: Instant) {
		let Some(neighbor) = self.neighbors.get_mut(&mac) else {
			return;
		};
		while let Some(event) = neighbor.link.poll_event() {
			match event {
				LinkEvent::Connected => {
					neighbor.stream = Some(Stream::new(self.handshake.clone()));
					neighbor.since = now;
				}
				LinkEvent::Reset => {
					log::info!("ESP-NOW neighbor {} restarted", MacDisplay(&mac));
					if let Some(mut stream) = neighbor.stream.take() {
						stream.close(&mut self.conn);
					}
					neighbor.since = now;
				}
				LinkEvent::Failed => {
					self.drop_neighbor(mac, None);
					return;
				}
			}
		}
		let Some(stream) = &mut neighbor.stream else {
			return;
		};
		for message in messages {
			let was_handshaking = stream.is_handshaking();
			if let Err(error) = stream.receive(&mut self.conn, &self.password, message) {
				log::warn!("Peering with {} failed: {error}", MacDisplay(&mac));
				self.drop_neighbor(mac, Some(now + COOLDOWN));
				return;
			}
			if was_handshaking && let Some(key) = stream.key() {
				log::info!(
					"Connected to {} over ESP-NOW from {}",
					Ipv6Addr::from(yggdrasil_proto::address::addr_for_key(&key).0),
					MacDisplay(&mac)
				);
			}
		}
	}

	/// Moves routing bytes from petrified into a neighbor's link.
	fn pump_neighbor(conn: &mut PacketConn, neighbor: &mut Neighbor) {
		let Some(stream) = &mut neighbor.stream else {
			return;
		};
		loop {
			let data = stream.transmit(conn);
			if data.is_empty() {
				return;
			}
			let n = data.len().min(PeerLink::max_payload());
			if neighbor.link.send(&data[..n]).is_err() {
				return;
			}
			stream.consume(conn, n);
		}
	}

	fn drop_neighbor(&mut self, mac: Mac, cooldown_until: Option<Instant>) {
		if let Some(neighbor) = self.neighbors.remove(&mac)
			&& let Some(mut stream) = neighbor.stream
		{
			stream.close(&mut self.conn);
		}
		if let Some(until) = cooldown_until {
			self.cooldown.insert(mac, until);
		}
	}

	/// Acts on everything petrified produced, then moves outgoing bytes into links.
	fn process(&mut self) {
		let now = self.now();
		while let Some(Event::PeerClosed { peer, error }) = self.conn.poll_event() {
			let neighbor = self.neighbors.iter().find_map(|(mac, neighbor)| {
				let stream = neighbor.stream.as_ref()?;
				(stream.peer() == Some(peer)).then_some(*mac)
			});
			if let Some(mac) = neighbor {
				log::info!("Disconnected from {}: {error:?}", MacDisplay(&mac));
				if let Some(stream) = self
					.neighbors
					.get_mut(&mac)
					.and_then(|neighbor| neighbor.stream.as_mut())
				{
					stream.mark_closed();
				}
				self.drop_neighbor(mac, None);
			}
			for entry in self.streams.values_mut() {
				if entry.stream.peer() == Some(peer) {
					entry.stream.mark_closed();
				}
			}
		}
		while let Some(key) = self.conn.network_mut().poll_path_notify() {
			for packet in self.keys.update(key, now) {
				send_traffic(&mut self.conn, &key, &packet);
			}
		}
		while let Some((from, packet)) = self.conn.recv() {
			match packet.split_first() {
				Some((&TYPE_SESSION_TRAFFIC, ip)) => self.handle_ipv6(from, ip, now),
				Some((&TYPE_SESSION_PROTO, body)) => {
					let view = View(&self.conn);
					if let requests::Incoming::Reply(reply) =
						requests::handle(body, &self.nodeinfo, &view)
					{
						let _ = self.conn.write_to(&reply, &from);
					}
				}
				_ => {}
			}
		}
		for neighbor in self.neighbors.values_mut() {
			Self::pump_neighbor(&mut self.conn, neighbor);
		}
	}

	fn handle_ipv6(&mut self, from: PublicKey, packet: &[u8], now: Instant) {
		let (verdict, released) = self.keys.incoming(from, packet, self.mtu, now);
		for held in released {
			send_traffic(&mut self.conn, &from, &held);
		}
		match verdict {
			Incoming::Deliver => match echo_reply(packet, &self.address()) {
				Some(reply) => {
					let _ = self.route_ipv6(&reply);
				}
				None => self.queue_inbox(packet.to_vec()),
			},
			Incoming::TooBig(reply) => {
				let _ = self.route_ipv6(&reply);
			}
			Incoming::Drop => {}
		}
	}

	fn queue_inbox(&mut self, packet: Vec<u8>) {
		self.inbox_bytes += packet.len();
		self.inbox.push_back(packet);
		while self.inbox_bytes > self.max_inbox_bytes {
			let Some(dropped) = self.inbox.pop_front() else {
				break;
			};
			self.inbox_bytes -= dropped.len();
		}
	}
}

fn send_traffic(conn: &mut PacketConn, key: &PublicKey, packet: &[u8]) {
	let mut message = Vec::with_capacity(packet.len() + 1);
	message.push(TYPE_SESSION_TRAFFIC);
	message.extend_from_slice(packet);
	let _ = conn.write_to(&message, key);
}

/// The reply to an ICMPv6 echo request addressed to `own`, if `packet` is one.
fn echo_reply(packet: &[u8], own: &Address) -> Option<Vec<u8>> {
	const ICMP_HEADER_LEN: usize = 4;
	if packet.len() < IPV6_HEADER_LEN + ICMP_HEADER_LEN
		|| packet[6] != NEXT_HEADER_ICMPV6
		|| packet[IPV6_HEADER_LEN] != ICMPV6_ECHO_REQUEST
		|| packet[24..40] != own.0
	{
		return None;
	}
	let payload_len = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
	let end = (IPV6_HEADER_LEN + payload_len).min(packet.len());
	let body = packet.get(IPV6_HEADER_LEN + ICMP_HEADER_LEN..end)?;
	let source: [u8; 16] = packet[8..24].try_into().ok()?;
	Some(create_icmpv6(
		Ipv6Addr::from(source),
		Ipv6Addr::from(own.0),
		ICMPV6_ECHO_REPLY,
		0,
		body,
	))
}

struct View<'a>(&'a PacketConn);

impl NodeView for View<'_> {
	fn public_key(&self) -> PublicKey {
		self.0.public_key()
	}

	fn routing_entries(&self) -> u64 {
		self.0.network().debug().get_self().routing_entries
	}

	fn peer_keys(&self) -> Vec<PublicKey> {
		self.0
			.network()
			.debug()
			.get_peers()
			.into_iter()
			.map(|peer| peer.key)
			.collect()
	}

	fn tree_keys(&self) -> Vec<PublicKey> {
		self.0
			.network()
			.debug()
			.get_tree()
			.into_iter()
			.map(|entry| entry.key)
			.collect()
	}

	fn mtu(&self) -> u64 {
		self.0.mtu()
	}
}

fn link_time(instant: Instant) -> espnow_reliable::Instant {
	let micros = instant.as_duration().as_micros();
	espnow_reliable::Instant::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
}

fn from_link_time(instant: espnow_reliable::Instant) -> Instant {
	Instant::from_duration(Duration::from_micros(instant.as_micros()))
}

struct MacDisplay<'a>(&'a Mac);

impl core::fmt::Display for MacDisplay<'_> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		let [a, b, c, d, e, g] = self.0;
		write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
	}
}
