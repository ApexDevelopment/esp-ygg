//! Voice calls over IAX2, between the phone and its UDP socket.

use alloc::vec;
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};

use smoltcp::iface::SocketHandle;
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint};
use yggdrasil_espnow::Instant;

use super::{Event, Overlay};
use crate::voice::iax2::{self, CallError, CallEvent, Phone};
use crate::voice::{CODECS, Codec};

impl Overlay {
	/// Starts taking calls on the IAX2 port, under `name`.
	pub fn start_phone(&mut self, name: &str, seed: u16) {
		let buffer =
			|| udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16 * 256]);
		let mut socket = udp::Socket::new(buffer(), buffer());
		socket.bind(iax2::PORT).expect("binding the IAX2 port");
		let handle = self.sockets.add(socket);
		self.phone = Some((Phone::new(name, CODECS, seed), handle));
	}

	/// Calls `to`. If that fails, an event says so, as for any call that ends.
	pub fn call(&mut self, to: Ipv6Addr, now: Instant) -> Result<(), CallError> {
		let result = self
			.phone()?
			.call(SocketAddrV6::new(to, iax2::PORT, 0, 0), now);
		if result.is_err() {
			let reason = "Already in a call".into();
			self.events.push_back(Event::Call(CallEvent::Ended(reason)));
		}
		result
	}

	pub fn answer(&mut self, now: Instant) -> Result<(), CallError> {
		self.phone()?.answer(now)
	}

	pub fn hang_up(&mut self, now: Instant) -> Result<(), CallError> {
		self.phone()?.hang_up(now)
	}

	/// The codec of the call that is up, if any.
	pub fn call_codec(&self) -> Option<Codec> {
		self.phone.as_ref().and_then(|(phone, _)| phone.codec())
	}

	/// Sends one frame of encoded voice. It goes out on the next poll.
	pub fn send_voice(&mut self, data: &[u8], now: Instant) {
		if let Ok(phone) = self.phone() {
			phone.send_voice(data, now);
		}
	}

	/// Voice that arrived, with its codec.
	pub fn poll_voice(&mut self) -> Option<(Codec, Vec<u8>)> {
		self.phone().ok()?.poll_voice()
	}

	/// Changes the name calls go out under.
	pub fn set_phone_name(&mut self, name: &str) {
		if let Ok(phone) = self.phone() {
			phone.set_name(name);
		}
	}

	fn phone(&mut self) -> Result<&mut Phone, CallError> {
		self.phone
			.as_mut()
			.map(|(phone, _)| phone)
			.ok_or(CallError::NoCall)
	}

	pub(super) fn phone_poll_at(&self) -> Option<Instant> {
		self.phone.as_ref().and_then(|(phone, _)| phone.poll_at())
	}

	/// Moves IAX2 datagrams between the phone and its socket.
	pub(super) fn run_phone(&mut self, now: Instant) {
		let Some((phone, handle)) = &mut self.phone else {
			return;
		};
		let socket = self.sockets.get_mut::<udp::Socket>(*handle);
		while let Ok((data, meta)) = socket.recv() {
			#[allow(irrefutable_let_patterns)]
			let IpAddress::Ipv6(address) = meta.endpoint.addr else {
				continue;
			};
			phone.receive(
				SocketAddrV6::new(address, meta.endpoint.port, 0, 0),
				data,
				now,
			);
		}
		if phone.poll_at().is_some_and(|due| due <= now) {
			phone.handle_timeout(now);
		}
		while let Some((to, datagram)) = phone.poll_transmit() {
			let endpoint = IpEndpoint::new(IpAddress::Ipv6(*to.ip()), to.port());
			// A full buffer drops the datagram; IAX2 resends what matters.
			let _ = socket.send_slice(&datagram, endpoint);
		}
		while let Some(event) = phone.poll_event() {
			self.events.push_back(Event::Call(event));
		}
	}
}

/// Keeps the phone's socket with the phone.
pub(super) type PhoneSlot = Option<(Phone, SocketHandle)>;
