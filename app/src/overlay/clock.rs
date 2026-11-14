//! The time of day: set from WiFi or asked of peers over Yggdrasil, and
//! told to peers that ask.

#[cfg(feature = "ntp")]
use alloc::collections::BTreeMap;
#[cfg(feature = "ntp")]
use alloc::vec;
#[cfg(feature = "ntp")]
use alloc::vec::Vec;
#[cfg(feature = "ntp")]
use core::net::Ipv6Addr;
use core::time::Duration;

#[cfg(feature = "ntp")]
use smoltcp::iface::{SocketHandle, SocketSet};
#[cfg(feature = "ntp")]
use smoltcp::socket::udp;
#[cfg(feature = "ntp")]
use smoltcp::wire::{IpAddress, IpEndpoint};
use yggdrasil_espnow::Instant;
#[cfg(feature = "ntp")]
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
#[cfg(feature = "ntp")]
use yggdrasil_espnow::{Node, Platform};

use super::Overlay;
#[cfg(feature = "ntp")]
use crate::sntp;

/// How often to ask peers for the time while the board does not know it.
#[cfg(feature = "ntp")]
const REQUEST_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(feature = "ntp")]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a synced time is trusted before asking again.
const RESYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The wall-clock time as of a moment on the board's own clock.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clock {
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

/// Asks peers for the time and answers them, over SNTP.
#[cfg(feature = "ntp")]
pub(super) struct TimeService {
	socket: SocketHandle,
	/// When each peer was last asked, so only replies we asked for count.
	requests: BTreeMap<Ipv6Addr, (Instant, sntp::Query)>,
	next_token: u64,
	pub next_request: Instant,
}

#[cfg(feature = "ntp")]
impl TimeService {
	pub fn new(sockets: &mut SocketSet<'static>, now: Instant, random_seed: u64) -> Self {
		let mut socket = udp::Socket::new(
			udp::PacketBuffer::new(
				vec![udp::PacketMetadata::EMPTY; 4],
				vec![0; 4 * sntp::MESSAGE_LEN],
			),
			udp::PacketBuffer::new(
				vec![udp::PacketMetadata::EMPTY; 8],
				vec![0; 8 * sntp::MESSAGE_LEN],
			),
		);
		socket.bind(sntp::PORT).expect("binding the time port");
		TimeService {
			socket: sockets.add(socket),
			requests: BTreeMap::new(),
			next_token: random_seed,
			next_request: now,
		}
	}
}

impl Overlay {
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
	#[cfg(feature = "ntp")]
	pub(super) fn keep_time<P: Platform>(&mut self, node: &Node<P>, now: Instant) {
		let handle = self.time.socket;
		let socket = self.sockets.get_mut::<udp::Socket>(handle);
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
				Some(sntp::Message::Reply(_)) => {
					let endpoint = core::net::SocketAddr::new(from.into(), meta.endpoint.port);
					if let Some((_, query)) = self.time.requests.get(&from)
						&& let Some(reply) = query.accept(endpoint, message)
					{
						replies.push((from, reply));
					}
				}
				None => {}
			}
		}
		for (from, reply) in replies {
			let asked = self
				.time
				.requests
				.remove(&from)
				.is_some_and(|(sent, _)| now.saturating_duration_since(sent) < REQUEST_TIMEOUT);
			if asked && reply.stratum < 15 {
				self.set_time(reply.unix_micros, reply.stratum + 1, now);
			}
		}
		let synced_recently = self
			.clock
			.is_some_and(|clock| now.saturating_duration_since(clock.synced) < RESYNC_INTERVAL);
		if synced_recently || now < self.time.next_request {
			return;
		}
		self.time.next_request = now + REQUEST_INTERVAL;
		self.time.requests.clear();
		let socket = self.sockets.get_mut::<udp::Socket>(handle);
		for peer in node.network().debug().get_peers() {
			let address = Ipv6Addr::from(addr_for_key(&peer.key).0);
			let endpoint = IpEndpoint::new(IpAddress::Ipv6(address), sntp::PORT);
			self.time.next_token = self.time.next_token.wrapping_add(1).max(1);
			let query = sntp::Query::new(
				core::net::SocketAddr::new(address.into(), sntp::PORT),
				self.time.next_token,
			);
			if socket.send_slice(&query.request(), endpoint).is_ok() {
				self.time.requests.insert(address, (now, query));
			}
		}
	}
}
