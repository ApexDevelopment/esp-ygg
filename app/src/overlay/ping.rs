//! Pings from the board, matched to their replies.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv6Addr;
use core::time::Duration;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::icmp;
use smoltcp::wire::{Icmpv6Packet, Icmpv6Repr, IpAddress};
use yggdrasil_espnow::Instant;

use super::{Event, Overlay, SendError};

/// How long to wait for a ping reply.
pub const PING_TIMEOUT: Duration = Duration::from_secs(5);
const PING_DATA: &[u8] = b"esp-ygg ping";

pub(super) struct Pinger {
	socket: SocketHandle,
	ident: u16,
	next_sequence: u16,
	/// Where each unanswered ping went, and when.
	pub sent: BTreeMap<u16, (Ipv6Addr, Instant)>,
}

impl Pinger {
	pub fn new(sockets: &mut SocketSet<'static>, ident: u16) -> Self {
		let mut socket = icmp::Socket::new(
			icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 1024]),
			icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 1024]),
		);
		socket
			.bind(icmp::Endpoint::Ident(ident))
			.expect("binding the ping socket");
		Pinger {
			socket: sockets.add(socket),
			ident,
			next_sequence: 0,
			sent: BTreeMap::new(),
		}
	}
}

impl Overlay {
	/// Sends one ping to `target` and returns its sequence number. The reply
	/// or timeout arrives as an [`Event`].
	pub fn ping(&mut self, target: Ipv6Addr, now: Instant) -> Result<u16, SendError> {
		let sequence = self.pinger.next_sequence;
		let repr = Icmpv6Repr::EchoRequest {
			ident: self.pinger.ident,
			seq_no: sequence,
			data: PING_DATA,
		};
		let source = self.address;
		let socket = self.sockets.get_mut::<icmp::Socket>(self.pinger.socket);
		let buffer = socket
			.send(repr.buffer_len(), IpAddress::Ipv6(target))
			.map_err(|_| SendError::Busy)?;
		let mut packet = Icmpv6Packet::new_unchecked(buffer);
		repr.emit(
			&source,
			&target,
			&mut packet,
			&ChecksumCapabilities::default(),
		);
		self.pinger.sent.insert(sequence, (target, now));
		self.pinger.next_sequence = sequence.wrapping_add(1);
		Ok(sequence)
	}

	pub(super) fn receive_pings(&mut self, now: Instant) {
		let socket = self.sockets.get_mut::<icmp::Socket>(self.pinger.socket);
		while let Ok((data, IpAddress::Ipv6(from))) = socket.recv() {
			let packet = Icmpv6Packet::new_unchecked(data);
			let checksums = ChecksumCapabilities::default();
			let Ok(Icmpv6Repr::EchoReply { ident, seq_no, .. }) =
				Icmpv6Repr::parse(&from, &self.address, &packet, &checksums)
			else {
				continue;
			};
			if ident != self.pinger.ident {
				continue;
			}
			// A reply from another address is not the one we asked for.
			if self
				.pinger
				.sent
				.get(&seq_no)
				.is_some_and(|(target, _)| *target == from)
				&& let Some((_, sent)) = self.pinger.sent.remove(&seq_no)
			{
				self.events.push_back(Event::PingReply {
					from,
					sequence: seq_no,
					round_trip: now.saturating_duration_since(sent),
				});
			}
		}
	}

	pub(super) fn expire_pings(&mut self, now: Instant) {
		let expired: Vec<u16> = self
			.pinger
			.sent
			.iter()
			.filter(|(_, (_, sent))| now >= *sent + PING_TIMEOUT)
			.map(|(sequence, _)| *sequence)
			.collect();
		for sequence in expired {
			if let Some((target, _)) = self.pinger.sent.remove(&sequence) {
				self.events
					.push_back(Event::PingTimeout { target, sequence });
			}
		}
	}
}
