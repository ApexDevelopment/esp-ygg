//! A TCP/IP stack on the Yggdrasil overlay, with the services every board
//! runs: the HTTP status page, the UDP console and ping.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::time::Duration;

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::{icmp, tcp, udp};
use smoltcp::wire::{Icmpv6Packet, Icmpv6Repr, IpAddress, IpEndpoint, IpListenEndpoint};
use yggdrasil_espnow::{Instant, Node, Platform};

use crate::device::{MTU, Queues, interface};
use crate::http::{self, BoardInfo};
use crate::sntp;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

pub const HTTP_PORT: u16 = 80;
/// The port the UDP console listens on.
pub const CONSOLE_PORT: u16 = 4242;
/// How long to wait for a ping reply.
pub const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// How often to ask peers for the time while the board does not know it.
const TIME_REQUEST_INTERVAL: Duration = Duration::from_secs(30);
const TIME_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a synced time is trusted before asking again.
const RESYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The wall-clock time as of a moment on the board's own clock.
#[derive(Clone, Copy, Debug)]
struct Clock {
	unix_micros: u64,
	synced: Instant,
	stratum: u8,
}

impl Clock {
	fn at(&self, now: Instant) -> u64 {
		let elapsed = now.saturating_duration_since(self.synced).as_micros();
		self.unix_micros + u64::try_from(elapsed).unwrap_or(u64::MAX)
	}
}

const HTTP_SOCKETS: usize = 2;
const PING_DATA: &[u8] = b"esp-ygg ping";

/// Something a service wants the user to see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	Datagram {
		from: SocketAddrV6,
		data: Vec<u8>,
	},
	PingReply {
		from: Ipv6Addr,
		sequence: u16,
		round_trip: Duration,
	},
	PingTimeout {
		target: Ipv6Addr,
		sequence: u16,
	},
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
	/// The data does not fit in one packet.
	TooLarge,
	/// Too much is already waiting to be sent.
	Busy,
}

struct HttpConnection {
	socket: SocketHandle,
	request: Vec<u8>,
	response: Vec<u8>,
	sent: usize,
}

pub struct Overlay {
	address: Ipv6Addr,
	interface: Interface,
	device: Queues,
	sockets: SocketSet<'static>,
	http: Vec<HttpConnection>,
	console: SocketHandle,
	ping: SocketHandle,
	ping_ident: u16,
	next_sequence: u16,
	pings: BTreeMap<u16, (Ipv6Addr, Instant)>,
	events: VecDeque<Event>,
	time: SocketHandle,
	clock: Option<Clock>,
	/// When each time server was last asked, so only replies we asked for count.
	time_requests: BTreeMap<Ipv6Addr, Instant>,
	next_time_request: Instant,
}

impl Overlay {
	/// `random_seed` should differ between boots, for TCP sequence numbers.
	/// Without `status_page`, port 80 is closed.
	pub fn new<P: Platform>(node: &Node<P>, random_seed: u64, status_page: bool) -> Self {
		let mut device = Queues::default();
		let now = to_smoltcp(node.now());
		let interface = interface(&node.address(), &mut device, random_seed, now);
		let mut sockets = SocketSet::new(Vec::new());
		let http_sockets = if status_page { HTTP_SOCKETS } else { 0 };
		let http = (0..http_sockets)
			.map(|_| {
				let socket = tcp::Socket::new(
					tcp::SocketBuffer::new(vec![0; http::MAX_REQUEST]),
					tcp::SocketBuffer::new(vec![0; 4096]),
				);
				HttpConnection {
					socket: sockets.add(socket),
					request: Vec::new(),
					response: Vec::new(),
					sent: 0,
				}
			})
			.collect();
		let mut console = udp::Socket::new(
			udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 4 * MTU]),
			udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 4 * MTU]),
		);
		console
			.bind(CONSOLE_PORT)
			.expect("binding the console port");
		let ping_ident = (random_seed >> 16) as u16;
		let mut ping = icmp::Socket::new(
			icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 1024]),
			icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 1024]),
		);
		let mut time = udp::Socket::new(
			udp::PacketBuffer::new(
				vec![udp::PacketMetadata::EMPTY; 4],
				vec![0; 4 * sntp::MESSAGE_LEN],
			),
			udp::PacketBuffer::new(
				vec![udp::PacketMetadata::EMPTY; 8],
				vec![0; 8 * sntp::MESSAGE_LEN],
			),
		);
		time.bind(sntp::PORT).expect("binding the time port");
		ping.bind(icmp::Endpoint::Ident(ping_ident))
			.expect("binding the ping socket");
		Overlay {
			address: Ipv6Addr::from(node.address().0),
			interface,
			device,
			http,
			console: sockets.add(console),
			time: sockets.add(time),
			ping: sockets.add(ping),
			sockets,
			ping_ident,
			next_sequence: 0,
			pings: BTreeMap::new(),
			events: VecDeque::new(),
			clock: None,
			time_requests: BTreeMap::new(),
			next_time_request: node.now(),
		}
	}

	/// Moves packets between the node and the stack and runs the services.
	/// Call it after every call to the node, and when [`Overlay::poll_at`] passes.
	pub fn poll<P: Platform>(&mut self, node: &mut Node<P>, board: &BoardInfo) {
		while let Some(packet) = node.recv_ipv6() {
			self.device.received.push_back(packet);
		}
		let now = node.now();
		self.interface
			.poll(to_smoltcp(now), &mut self.device, &mut self.sockets);
		self.serve_http(node, board);
		self.receive_datagrams();
		self.receive_pings(now);
		self.expire_pings(now);
		self.keep_time(node, now);
		// Send whatever the services just queued.
		self.interface
			.poll(to_smoltcp(now), &mut self.device, &mut self.sockets);
		while let Some(packet) = self.device.outgoing.pop_front() {
			let _ = node.send_ipv6(&packet);
		}
	}

	/// When [`Overlay::poll`] next needs to run, if no packets arrive first.
	pub fn poll_at(&mut self, now: Instant) -> Option<Instant> {
		let stack = self
			.interface
			.poll_at(to_smoltcp(now), &self.sockets)
			.map(from_smoltcp);
		let pings = self.pings.values().map(|(_, sent)| *sent + PING_TIMEOUT);
		stack
			.into_iter()
			.chain(pings)
			.chain([self.next_time_request])
			.min()
	}

	pub fn poll_event(&mut self) -> Option<Event> {
		self.events.pop_front()
	}

	/// Sends `data` to a UDP port. It goes out on the next [`Overlay::poll`].
	pub fn send_datagram(&mut self, to: SocketAddrV6, data: &[u8]) -> Result<(), SendError> {
		let endpoint = IpEndpoint::new(IpAddress::Ipv6(*to.ip()), to.port());
		let console = self.sockets.get_mut::<udp::Socket>(self.console);
		console
			.send_slice(data, endpoint)
			.map_err(|error| match error {
				udp::SendError::BufferFull => SendError::Busy,
				_ => SendError::TooLarge,
			})
	}

	/// Sends one ping to `target` and returns its sequence number. The reply
	/// or timeout arrives as an [`Event`].
	pub fn ping(&mut self, target: Ipv6Addr, now: Instant) -> Result<u16, SendError> {
		let sequence = self.next_sequence;
		let repr = Icmpv6Repr::EchoRequest {
			ident: self.ping_ident,
			seq_no: sequence,
			data: PING_DATA,
		};
		let source = self.address;
		let destination = target;
		let ping = self.sockets.get_mut::<icmp::Socket>(self.ping);
		let buffer = ping
			.send(repr.buffer_len(), IpAddress::Ipv6(destination))
			.map_err(|_| SendError::Busy)?;
		let mut packet = Icmpv6Packet::new_unchecked(buffer);
		repr.emit(
			&source,
			&destination,
			&mut packet,
			&ChecksumCapabilities::default(),
		);
		self.pings.insert(sequence, (target, now));
		self.next_sequence = sequence.wrapping_add(1);
		Ok(sequence)
	}

	/// Microseconds since the Unix epoch, once the board knows the time.
	pub fn unix_micros(&self, now: Instant) -> Option<u64> {
		self.clock.map(|clock| clock.at(now))
	}

	/// Sets the time from a source `stratum` steps from a reference clock,
	/// such as an NTP server reached over WiFi, unless a closer source
	/// already set it recently.
	pub fn set_time(&mut self, unix_micros: u64, stratum: u8, now: Instant) {
		let replace = self.clock.is_none_or(|clock| {
			stratum < clock.stratum
				|| now.saturating_duration_since(clock.synced) >= RESYNC_INTERVAL
		});
		if replace {
			self.clock = Some(Clock {
				unix_micros,
				synced: now,
				stratum,
			});
		}
	}

	/// Answers time requests once the board knows the time, and asks its
	/// direct peers until it does, then once in a while.
	fn keep_time<P: Platform>(&mut self, node: &Node<P>, now: Instant) {
		let socket = self.sockets.get_mut::<udp::Socket>(self.time);
		let mut replies = Vec::new();
		while let Ok((message, meta)) = socket.recv() {
			#[allow(irrefutable_let_patterns)]
			let IpAddress::Ipv6(from) = meta.endpoint.addr else {
				continue;
			};
			match sntp::parse(message) {
				Some(sntp::Message::Request) => {
					if let Some(clock) = self.clock {
						let reply = sntp::reply(message, clock.at(now), clock.stratum);
						let _ = socket.send_slice(&reply, meta.endpoint);
					}
				}
				Some(sntp::Message::Reply(reply)) => replies.push((from, reply)),
				None => {}
			}
		}
		for (from, reply) in replies {
			let asked = self
				.time_requests
				.remove(&from)
				.is_some_and(|sent| now.saturating_duration_since(sent) < TIME_REQUEST_TIMEOUT);
			if asked && reply.stratum < 15 {
				self.set_time(reply.unix_micros, reply.stratum + 1, now);
			}
		}
		let synced_recently = self
			.clock
			.is_some_and(|clock| now.saturating_duration_since(clock.synced) < RESYNC_INTERVAL);
		if synced_recently || now < self.next_time_request {
			return;
		}
		self.next_time_request = now + TIME_REQUEST_INTERVAL;
		self.time_requests.clear();
		let socket = self.sockets.get_mut::<udp::Socket>(self.time);
		for peer in node.network().debug().get_peers() {
			let address = Ipv6Addr::from(addr_for_key(&peer.key).0);
			let endpoint = IpEndpoint::new(IpAddress::Ipv6(address), sntp::PORT);
			if socket.send_slice(&sntp::request(), endpoint).is_ok() {
				self.time_requests.insert(address, now);
			}
		}
	}

	fn serve_http<P: Platform>(&mut self, node: &Node<P>, board: &BoardInfo) {
		for connection in &mut self.http {
			let socket = self.sockets.get_mut::<tcp::Socket>(connection.socket);
			if !socket.is_open() {
				connection.request.clear();
				connection.response.clear();
				connection.sent = 0;
				let _ = socket.listen(IpListenEndpoint::from(HTTP_PORT));
				continue;
			}
			if connection.response.is_empty() {
				let _ = socket.recv(|data| {
					connection.request.extend_from_slice(data);
					(data.len(), ())
				});
				if http::is_complete(&connection.request) {
					connection.response = http::respond(&connection.request, node, board);
				}
			}
			if connection.sent < connection.response.len() {
				if let Ok(sent) = socket.send_slice(&connection.response[connection.sent..]) {
					connection.sent += sent;
				}
				if connection.sent == connection.response.len() {
					socket.close();
				}
			}
		}
	}

	fn receive_datagrams(&mut self) {
		let console = self.sockets.get_mut::<udp::Socket>(self.console);
		while let Ok((data, meta)) = console.recv() {
			// Other builds of smoltcp in the firmware also speak IPv4.
			#[allow(irrefutable_let_patterns)]
			let IpAddress::Ipv6(address) = meta.endpoint.addr else {
				continue;
			};
			let from = SocketAddrV6::new(address, meta.endpoint.port, 0, 0);
			self.events.push_back(Event::Datagram {
				from,
				data: data.to_vec(),
			});
		}
	}

	fn receive_pings(&mut self, now: Instant) {
		let ping = self.sockets.get_mut::<icmp::Socket>(self.ping);
		while let Ok((data, IpAddress::Ipv6(from))) = ping.recv() {
			let packet = Icmpv6Packet::new_unchecked(data);
			let source = from;
			let destination = self.address;
			let checksums = ChecksumCapabilities::default();
			let Ok(Icmpv6Repr::EchoReply { ident, seq_no, .. }) =
				Icmpv6Repr::parse(&source, &destination, &packet, &checksums)
			else {
				continue;
			};
			if ident != self.ping_ident {
				continue;
			}
			let from = source;
			// A reply from another address is not the one we asked for.
			if self
				.pings
				.get(&seq_no)
				.is_some_and(|(target, _)| *target == from)
				&& let Some((_, sent)) = self.pings.remove(&seq_no)
			{
				self.events.push_back(Event::PingReply {
					from,
					sequence: seq_no,
					round_trip: now.saturating_duration_since(sent),
				});
			}
		}
	}

	fn expire_pings(&mut self, now: Instant) {
		let expired: Vec<u16> = self
			.pings
			.iter()
			.filter(|(_, (_, sent))| now >= *sent + PING_TIMEOUT)
			.map(|(sequence, _)| *sequence)
			.collect();
		for sequence in expired {
			if let Some((target, _)) = self.pings.remove(&sequence) {
				self.events
					.push_back(Event::PingTimeout { target, sequence });
			}
		}
	}
}

fn to_smoltcp(instant: Instant) -> smoltcp::time::Instant {
	let micros = instant.as_duration().as_micros();
	smoltcp::time::Instant::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn from_smoltcp(instant: smoltcp::time::Instant) -> Instant {
	Instant::from_duration(Duration::from_micros(instant.total_micros().max(0) as u64))
}
