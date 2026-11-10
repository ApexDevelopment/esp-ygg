//! The UDP console: text to and from a port anyone can send to.

use alloc::vec;
use core::net::SocketAddrV6;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint};

use super::{Event, Overlay, SendError};
use crate::device::MTU;

/// The port the UDP console listens on.
pub const CONSOLE_PORT: u16 = 4242;

pub(super) fn socket(sockets: &mut SocketSet<'static>) -> SocketHandle {
	let buffer = || udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2 * MTU]);
	let mut socket = udp::Socket::new(buffer(), buffer());
	socket.bind(CONSOLE_PORT).expect("binding the console port");
	sockets.add(socket)
}

impl Overlay {
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

	pub(super) fn receive_datagrams(&mut self) {
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
}
