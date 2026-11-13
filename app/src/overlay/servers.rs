//! The board's status pages, over HTTP and Gopher.

use alloc::vec::Vec;
#[cfg(feature = "gopher")]
use core::net::Ipv6Addr;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::wire::IpListenEndpoint;
#[cfg(feature = "gopher")]
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Node, Platform};

use super::Overlay;
#[cfg(feature = "gopher")]
use crate::gopher;
use crate::http::{self, BoardInfo};

pub const HTTP_PORT: u16 = 80;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
	#[cfg(feature = "http")]
	Http,
	#[cfg(feature = "gopher")]
	Gopher,
}

impl Protocol {
	fn port(self) -> u16 {
		match self {
			#[cfg(feature = "http")]
			Protocol::Http => HTTP_PORT,
			#[cfg(feature = "gopher")]
			Protocol::Gopher => gopher::PORT,
		}
	}
}

/// One connection a status server can take at a time.
pub(super) struct ServerConnection {
	protocol: Protocol,
	socket: SocketHandle,
	request: Vec<u8>,
	response: Vec<u8>,
	sent: usize,
}

/// A listening socket for each connection the servers can take at once.
pub(super) fn listen(sockets: &mut SocketSet<'static>) -> Vec<ServerConnection> {
	let protocols = [
		#[cfg(feature = "http")]
		Protocol::Http,
		#[cfg(feature = "http")]
		Protocol::Http,
		#[cfg(feature = "gopher")]
		Protocol::Gopher,
	];
	protocols
		.into_iter()
		.map(|protocol| {
			// Requests and responses are gathered elsewhere, so the socket
			// only needs room for what is in flight.
			let socket = tcp::Socket::new(
				tcp::SocketBuffer::new(alloc::vec![0; 512]),
				tcp::SocketBuffer::new(alloc::vec![0; 1024]),
			);
			ServerConnection {
				protocol,
				socket: sockets.add(socket),
				request: Vec::new(),
				response: Vec::new(),
				sent: 0,
			}
		})
		.collect()
}

impl Overlay {
	/// Opens the status pages' ports, or closes them.
	pub fn set_status_pages(&mut self, on: bool) {
		if on && self.servers.is_empty() {
			self.servers = listen(&mut self.sockets);
		} else if !on {
			for connection in self.servers.drain(..) {
				self.sockets.remove(connection.socket);
			}
		}
	}

	pub(super) fn serve<P: Platform>(&mut self, node: &Node<P>, board: &BoardInfo) {
		for connection in &mut self.servers {
			let socket = self.sockets.get_mut::<tcp::Socket>(connection.socket);
			if !socket.is_open() {
				// Drop rather than clear, so idle connections hold no memory.
				connection.request = Vec::new();
				connection.response = Vec::new();
				connection.sent = 0;
				let port = connection.protocol.port();
				let _ = socket.listen(IpListenEndpoint::from(port));
				continue;
			}
			if connection.response.is_empty() {
				let _ = socket.recv(|data| {
					connection.request.extend_from_slice(data);
					(data.len(), ())
				});
				if connection.request.len() > http::MAX_REQUEST {
					socket.abort();
					continue;
				}
				connection.response = match connection.protocol {
					#[cfg(feature = "http")]
					Protocol::Http if http::is_complete(&connection.request) => {
						http::respond(&connection.request, node, board)
					}
					#[cfg(feature = "gopher")]
					Protocol::Gopher if gopher_request_complete(&connection.request) => {
						gopher_response(&connection.request, node, board)
					}
					_ => Vec::new(),
				};
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
}

#[cfg(feature = "gopher")]
fn gopher_request_complete(request: &[u8]) -> bool {
	request.contains(&b'\n') || request.len() >= http::MAX_REQUEST
}

/// The board's Gopher menu for an empty selector, and an error otherwise.
#[cfg(feature = "gopher")]
fn gopher_response<P: Platform>(request: &[u8], node: &Node<P>, board: &BoardInfo) -> Vec<u8> {
	let selector = request
		.split(|&byte| byte == b'\r' || byte == b'\n')
		.next()
		.unwrap_or_default();
	if !selector.is_empty() && selector != b"/" {
		let mut response = gopher::menu_line('3', "Not found", "", "error.host", 1).into_bytes();
		response.extend_from_slice(b".\r\n");
		return response;
	}
	let peers: Vec<Ipv6Addr> = node
		.network()
		.debug()
		.get_peers()
		.into_iter()
		.map(|peer| Ipv6Addr::from(addr_for_key(&peer.key).0))
		.collect();
	gopher::status_menu(&http::summary(node, board), &peers)
}
