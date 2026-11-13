//! A TCP/IP stack on the Yggdrasil overlay, with the services the board
//! runs on it. Each service lives in a module of its own, and is left out
//! entirely when its feature is.

#[cfg(any(
	feature = "gopher",
	feature = "irc",
	feature = "gemini",
	feature = "share"
))]
mod clients;
mod clock;
#[cfg(feature = "console")]
mod console;
#[cfg(feature = "voice")]
mod phone;
mod ping;
#[cfg(any(feature = "http", feature = "gopher"))]
mod servers;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::Ipv6Addr;
#[cfg(any(feature = "console", feature = "voice"))]
use core::net::SocketAddrV6;
use core::time::Duration;

#[cfg(any(
	feature = "gopher",
	feature = "irc",
	feature = "gemini",
	feature = "share"
))]
use smoltcp::iface::SocketHandle;
use smoltcp::iface::{Interface, SocketSet};
use yggdrasil_espnow::{Instant, Node, Platform};

#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
pub use clients::Stream;
#[cfg(feature = "console")]
pub use console::CONSOLE_PORT;
pub use ping::PING_TIMEOUT;
#[cfg(any(feature = "http", feature = "gopher"))]
pub use servers::HTTP_PORT;

use crate::device::{Queues, interface};
use crate::hotspot::{Hotspot, Mac};
use crate::http::BoardInfo;
#[cfg(feature = "voice")]
use crate::voice::iax2::CallEvent;

/// Something a service wants the user to see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	#[cfg(feature = "console")]
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
	/// The response to [`Overlay::fetch`], up to its size limit.
	#[cfg(feature = "gopher")]
	Fetched {
		data: Vec<u8>,
	},
	/// The server could not be reached, or did not answer in time.
	#[cfg(feature = "gopher")]
	FetchFailed,
	/// The connection from [`Overlay::open_stream`] is ready for data.
	#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
	StreamOpened(Stream),
	#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
	StreamData {
		stream: Stream,
		data: Vec<u8>,
	},
	/// The stream ended, or could not be opened.
	#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
	StreamClosed(Stream),
	/// A peer connected to a stream the board listens on.
	#[cfg(feature = "share")]
	StreamAccepted {
		stream: Stream,
		from: Ipv6Addr,
	},
	#[cfg(feature = "voice")]
	Call(CallEvent),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
	/// The data does not fit in one packet.
	TooLarge,
	/// Too much is already waiting to be sent.
	Busy,
	/// The stream is not open.
	Unavailable,
}

pub struct Overlay {
	address: Ipv6Addr,
	interface: Interface,
	device: Queues,
	sockets: SocketSet<'static>,
	events: VecDeque<Event>,
	hotspot: Option<Hotspot>,
	pinger: ping::Pinger,
	clock: Option<clock::Clock>,
	#[cfg(feature = "ntp")]
	time: clock::TimeService,
	#[cfg(any(feature = "http", feature = "gopher"))]
	servers: Vec<servers::ServerConnection>,
	#[cfg(feature = "console")]
	console: smoltcp::iface::SocketHandle,
	/// Connections this board closed, kept until they finish closing.
	#[cfg(any(
		feature = "gopher",
		feature = "irc",
		feature = "gemini",
		feature = "share"
	))]
	closing: Vec<SocketHandle>,
	#[cfg(any(
		feature = "gopher",
		feature = "irc",
		feature = "gemini",
		feature = "share"
	))]
	next_local_port: u16,
	#[cfg(feature = "gopher")]
	fetch: Option<clients::Fetch>,
	#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
	streams: Vec<clients::StreamSlot>,
	#[cfg(feature = "voice")]
	phone: phone::PhoneSlot,
}

impl Overlay {
	/// `random_seed` should differ between boots, for TCP sequence numbers.
	/// Without `status_pages`, the HTTP and Gopher ports are closed.
	pub fn new<P: Platform>(node: &Node<P>, random_seed: u64, status_pages: bool) -> Self {
		#[cfg_attr(not(feature = "share"), allow(unused_mut))]
		let mut overlay = Overlay::without_listeners(node, random_seed, status_pages);
		#[cfg(feature = "share")]
		overlay.listen_for_share();
		overlay
	}

	fn without_listeners<P: Platform>(
		node: &Node<P>,
		random_seed: u64,
		status_pages: bool,
	) -> Self {
		let mut device = Queues::default();
		let now = to_smoltcp(node.now());
		let interface = interface(&node.address(), &mut device, random_seed, now);
		let mut sockets = SocketSet::new(Vec::new());
		#[cfg(not(any(feature = "http", feature = "gopher")))]
		let _ = status_pages;
		Overlay {
			address: Ipv6Addr::from(node.address().0),
			interface,
			device,
			events: VecDeque::new(),
			hotspot: None,
			pinger: ping::Pinger::new(&mut sockets, (random_seed >> 16) as u16),
			clock: None,
			#[cfg(feature = "ntp")]
			time: clock::TimeService::new(&mut sockets, node.now()),
			#[cfg(any(feature = "http", feature = "gopher"))]
			servers: if status_pages {
				servers::listen(&mut sockets)
			} else {
				Vec::new()
			},
			#[cfg(feature = "console")]
			console: console::socket(&mut sockets),
			#[cfg(any(
				feature = "gopher",
				feature = "irc",
				feature = "gemini",
				feature = "share"
			))]
			closing: Vec::new(),
			#[cfg(any(
				feature = "gopher",
				feature = "irc",
				feature = "gemini",
				feature = "share"
			))]
			next_local_port: clients::EPHEMERAL_PORTS.start + (random_seed as u16 % 1000),
			#[cfg(feature = "gopher")]
			fetch: None,
			#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
			streams: clients::stream_slots(),
			#[cfg(feature = "voice")]
			phone: None,
			sockets,
		}
	}

	/// The status pages are left out of this build.
	#[cfg(not(any(feature = "http", feature = "gopher")))]
	pub fn set_status_pages(&mut self, _on: bool) {}

	/// Moves packets between the node and the stack and runs the services.
	/// Call it after every call to the node, and when [`Overlay::poll_at`] passes.
	pub fn poll<P: Platform>(&mut self, node: &mut Node<P>, board: &BoardInfo) {
		let now = node.now();
		while let Some(packet) = node.recv_ipv6() {
			match &mut self.hotspot {
				Some(hotspot) if hotspot.in_subnet(&destination(&packet)) => hotspot.send(packet),
				_ => self.device.received.push_back(packet),
			}
		}
		if let Some(hotspot) = &mut self.hotspot
			&& hotspot.poll_at() <= now
		{
			hotspot.handle_timeout(now);
		}
		// Packets to this node come straight back, and their answers may
		// too, so a ping to itself goes round a few times.
		for _ in 0..LOOPBACK_ROUNDS {
			self.interface
				.poll(to_smoltcp(now), &mut self.device, &mut self.sockets);
			self.run_services(node, board, now);
			// Send whatever the services just queued.
			self.interface
				.poll(to_smoltcp(now), &mut self.device, &mut self.sockets);
			if !self.send_outgoing(node) {
				break;
			}
		}
	}

	/// Sends what the stack queued. Returns whether any of it was to this
	/// node, which the mesh cannot deliver, and so was kept to receive.
	fn send_outgoing<P: Platform>(&mut self, node: &mut Node<P>) -> bool {
		let own = self.address.octets();
		let mut looped = false;
		while let Some(packet) = self.device.outgoing.pop_front() {
			let to = destination(&packet);
			match &mut self.hotspot {
				_ if to == own => {
					self.device.received.push_back(packet);
					looped = true;
				}
				Some(hotspot) if hotspot.in_subnet(&to) => hotspot.send(packet),
				_ => {
					let _ = node.send_ipv6(&packet);
				}
			}
		}
		looped
	}

	#[cfg_attr(
		not(any(feature = "http", feature = "gopher", feature = "ntp")),
		allow(unused_variables)
	)]
	fn run_services<P: Platform>(&mut self, node: &Node<P>, board: &BoardInfo, now: Instant) {
		#[cfg(any(feature = "http", feature = "gopher"))]
		self.serve(node, board);
		#[cfg(feature = "gopher")]
		self.progress_fetch(now);
		#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
		self.progress_streams();
		#[cfg(any(
			feature = "gopher",
			feature = "irc",
			feature = "gemini",
			feature = "share"
		))]
		self.free_closed_sockets();
		#[cfg(feature = "console")]
		self.receive_datagrams();
		#[cfg(feature = "voice")]
		self.run_phone(now);
		self.receive_pings(now);
		self.expire_pings(now);
		#[cfg(feature = "ntp")]
		self.keep_time(node, now);
	}

	/// Starts routing for devices on the board's own WiFi network, whose
	/// access point has hardware address `mac`.
	pub fn start_hotspot<P: Platform>(&mut self, node: &Node<P>, mac: Mac) {
		self.hotspot = Some(Hotspot::new(mac, node.subnet().0, node.now()));
	}

	/// Takes a frame from the hotspot. Call [`Overlay::poll`] afterwards.
	pub fn receive_hotspot_frame<P: Platform>(&mut self, node: &mut Node<P>, frame: &[u8]) {
		let Some(hotspot) = &mut self.hotspot else {
			return;
		};
		let Some(packet) = hotspot.receive(frame, node.now()) else {
			return;
		};
		if Ipv6Addr::from(destination(&packet)) == self.address {
			self.device.received.push_back(packet);
		} else {
			let _ = node.send_ipv6(&packet);
		}
	}

	/// The next frame for the hotspot to send.
	pub fn poll_hotspot_frame(&mut self) -> Option<Vec<u8>> {
		self.hotspot.as_mut()?.poll_frame()
	}

	/// How many devices are using the hotspot, if it is on.
	pub fn hotspot_devices(&self) -> Option<usize> {
		self.hotspot.as_ref().map(Hotspot::devices)
	}

	/// When [`Overlay::poll`] next needs to run, if no packets arrive first.
	pub fn poll_at(&mut self, now: Instant) -> Option<Instant> {
		let stack = self
			.interface
			.poll_at(to_smoltcp(now), &self.sockets)
			.map(from_smoltcp);
		let pings = self
			.pinger
			.sent
			.values()
			.map(|(_, sent)| *sent + PING_TIMEOUT);
		let deadlines = [
			#[cfg(feature = "ntp")]
			Some(self.time.next_request),
			self.hotspot.as_ref().map(Hotspot::poll_at),
			#[cfg(feature = "voice")]
			self.phone_poll_at(),
			#[cfg(feature = "gopher")]
			self.fetch
				.as_ref()
				.map(|fetch| fetch.started + clients::FETCH_TIMEOUT),
		];
		stack
			.into_iter()
			.chain(pings)
			.chain(deadlines.into_iter().flatten())
			.min()
	}

	pub fn poll_event(&mut self) -> Option<Event> {
		self.events.pop_front()
	}
}

/// An IPv6 packet's destination address, or `::` if it is too short to have one.
/// How many times a packet to this node, then its answer, can go round in
/// one poll.
const LOOPBACK_ROUNDS: usize = 4;

fn destination(packet: &[u8]) -> [u8; 16] {
	packet
		.get(24..40)
		.and_then(|bytes| bytes.try_into().ok())
		.unwrap_or([0; 16])
}

fn to_smoltcp(instant: Instant) -> smoltcp::time::Instant {
	let micros = instant.as_duration().as_micros();
	smoltcp::time::Instant::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn from_smoltcp(instant: smoltcp::time::Instant) -> Instant {
	Instant::from_duration(Duration::from_micros(instant.total_micros().max(0) as u64))
}
