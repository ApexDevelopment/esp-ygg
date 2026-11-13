//! Connections the board opens to other servers: one-off fetches for Gopher,
//! and lasting streams for IRC and Gemini.

use alloc::vec;
use alloc::vec::Vec;
use core::net::SocketAddrV6;
#[cfg(feature = "gopher")]
use core::time::Duration;

use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint};
#[cfg(feature = "gopher")]
use yggdrasil_espnow::Instant;

use super::{Event, Overlay, SendError};

/// Local ports for connections this board opens.
pub(super) const EPHEMERAL_PORTS: core::ops::Range<u16> = 49152..65535;
/// How long a fetch may take, and how much of a response is kept.
#[cfg(feature = "gopher")]
pub(super) const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(feature = "gopher")]
const MAX_FETCH: usize = 32 * 1024;
/// How long a stream may go without an answer before it is given up.
#[cfg(any(feature = "irc", feature = "gemini"))]
const STREAM_TIMEOUT_SECS: u64 = 300;
/// How long a stream may take to connect.
#[cfg(any(feature = "irc", feature = "gemini"))]
const CONNECT_TIMEOUT_SECS: u64 = 20;
/// How much may wait to be sent on a stream.
#[cfg(any(feature = "irc", feature = "gemini"))]
const MAX_STREAM_BACKLOG: usize = 4096;

/// The lasting connections the board can hold, one for each client that
/// needs one, so they can stay open side by side.
#[cfg(any(feature = "irc", feature = "gemini"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
	#[cfg(feature = "irc")]
	Irc,
	#[cfg(feature = "gemini")]
	Gemini,
}

#[cfg(any(feature = "irc", feature = "gemini"))]
impl Stream {
	/// Each stream, with its receive buffer size.
	const ALL: &[(Stream, usize)] = &[
		#[cfg(feature = "irc")]
		(Stream::Irc, 2048),
		#[cfg(feature = "gemini")]
		(Stream::Gemini, 4096),
	];
}

#[cfg(any(feature = "irc", feature = "gemini"))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamState {
	Closed,
	Connecting,
	Open,
}

#[cfg(any(feature = "irc", feature = "gemini"))]
pub(super) struct StreamSlot {
	stream: Stream,
	receive_buffer: usize,
	/// Made when the stream opens, so a closed stream costs no memory.
	socket: Option<SocketHandle>,
	state: StreamState,
	outgoing: Vec<u8>,
}

#[cfg(any(feature = "irc", feature = "gemini"))]
pub(super) fn stream_slots() -> Vec<StreamSlot> {
	Stream::ALL
		.iter()
		.map(|&(stream, receive_buffer)| StreamSlot {
			stream,
			receive_buffer,
			socket: None,
			state: StreamState::Closed,
			outgoing: Vec::new(),
		})
		.collect()
}

/// A request to another server, and what has come back so far.
#[cfg(feature = "gopher")]
pub(super) struct Fetch {
	socket: SocketHandle,
	request: Vec<u8>,
	sent: bool,
	connected: bool,
	response: Vec<u8>,
	pub started: Instant,
}

impl Overlay {
	/// Fetches from a TCP server: sends `request`, then collects everything
	/// until the server closes the connection, as Gopher does. The result
	/// arrives as an [`Event`]. A new fetch replaces one in progress.
	#[cfg(feature = "gopher")]
	pub fn fetch(
		&mut self,
		to: SocketAddrV6,
		request: Vec<u8>,
		now: Instant,
	) -> Result<(), SendError> {
		if let Some(old) = self.fetch.take() {
			self.sockets.get_mut::<tcp::Socket>(old.socket).abort();
			self.closing.push(old.socket);
		}
		let socket = self.connect(to, 4096)?;
		self.fetch = Some(Fetch {
			socket,
			request,
			sent: false,
			connected: false,
			response: Vec::new(),
			started: now,
		});
		Ok(())
	}

	/// Opens a lasting TCP connection, replacing the one `stream` had open.
	/// Its progress arrives as [`Event`]s.
	#[cfg(any(feature = "irc", feature = "gemini"))]
	pub fn open_stream(&mut self, stream: Stream, to: SocketAddrV6) -> Result<(), SendError> {
		let slot = self.slot(stream);
		let receive_buffer = slot.receive_buffer;
		if let Some(old) = slot.socket.take() {
			self.sockets.get_mut::<tcp::Socket>(old).abort();
			self.closing.push(old);
		}
		let socket = self.connect(to, receive_buffer)?;
		let lasting = self.sockets.get_mut::<tcp::Socket>(socket);
		lasting.set_timeout(Some(smoltcp::time::Duration::from_secs(
			CONNECT_TIMEOUT_SECS,
		)));
		lasting.set_keep_alive(Some(smoltcp::time::Duration::from_secs(60)));
		let slot = self.slot(stream);
		slot.socket = Some(socket);
		slot.state = StreamState::Connecting;
		slot.outgoing.clear();
		Ok(())
	}

	/// Queues data for an open stream.
	#[cfg(any(feature = "irc", feature = "gemini"))]
	pub fn stream_send(&mut self, stream: Stream, data: &[u8]) -> Result<(), SendError> {
		let slot = self.slot(stream);
		if slot.state == StreamState::Closed {
			return Err(SendError::Unavailable);
		}
		if slot.outgoing.len() + data.len() > MAX_STREAM_BACKLOG {
			return Err(SendError::Busy);
		}
		slot.outgoing.extend_from_slice(data);
		Ok(())
	}

	/// Closes a stream once what is queued has been sent.
	#[cfg(any(feature = "irc", feature = "gemini"))]
	pub fn close_stream(&mut self, stream: Stream) {
		if let Some(socket) = self.slot(stream).socket {
			self.sockets.get_mut::<tcp::Socket>(socket).close();
		}
	}

	#[cfg(any(feature = "irc", feature = "gemini"))]
	fn slot(&mut self, stream: Stream) -> &mut StreamSlot {
		self.streams
			.iter_mut()
			.find(|slot| slot.stream == stream)
			.expect("every stream has a slot")
	}

	/// Makes a socket and starts connecting it to `to`.
	fn connect(
		&mut self,
		to: SocketAddrV6,
		receive_buffer: usize,
	) -> Result<SocketHandle, SendError> {
		let local_port = self.next_port();
		let socket = self.sockets.add(tcp::Socket::new(
			tcp::SocketBuffer::new(vec![0; receive_buffer]),
			tcp::SocketBuffer::new(vec![0; 1024]),
		));
		let remote = IpEndpoint::new(IpAddress::Ipv6(*to.ip()), to.port());
		let connected = self.sockets.get_mut::<tcp::Socket>(socket).connect(
			self.interface.context(),
			remote,
			local_port,
		);
		if connected.is_err() {
			self.sockets.remove(socket);
			return Err(SendError::Busy);
		}
		Ok(socket)
	}

	fn next_port(&mut self) -> u16 {
		let port = self.next_local_port;
		self.next_local_port = port
			.checked_add(1)
			.filter(|port| EPHEMERAL_PORTS.contains(port))
			.unwrap_or(EPHEMERAL_PORTS.start);
		port
	}

	/// Frees the memory of connections that have finished closing.
	pub(super) fn free_closed_sockets(&mut self) {
		let sockets = &mut self.sockets;
		self.closing.retain(|&handle| {
			let open = sockets.get::<tcp::Socket>(handle).is_open();
			if !open {
				sockets.remove(handle);
			}
			open
		});
	}

	#[cfg(any(feature = "irc", feature = "gemini"))]
	pub(super) fn progress_streams(&mut self) {
		for slot in &mut self.streams {
			let Some(handle) = slot.socket else {
				continue;
			};
			let stream = slot.stream;
			let socket = self.sockets.get_mut::<tcp::Socket>(handle);
			if slot.state == StreamState::Connecting && socket.may_send() {
				slot.state = StreamState::Open;
				socket.set_timeout(Some(smoltcp::time::Duration::from_secs(
					STREAM_TIMEOUT_SECS,
				)));
				self.events.push_back(Event::StreamOpened(stream));
			}
			if slot.state == StreamState::Open
				&& !slot.outgoing.is_empty()
				&& let Ok(sent) = socket.send_slice(&slot.outgoing)
			{
				slot.outgoing.drain(..sent);
			}
			while socket.can_recv() {
				let mut data = Vec::new();
				let _ = socket.recv(|bytes| {
					data.extend_from_slice(bytes);
					(bytes.len(), ())
				});
				self.events.push_back(Event::StreamData { stream, data });
			}
			// The server finished, so finish too.
			if slot.state == StreamState::Open && !socket.may_recv() && socket.may_send() {
				socket.close();
			}
			if !socket.is_open() {
				slot.state = StreamState::Closed;
				slot.socket = None;
				slot.outgoing = Vec::new();
				self.sockets.remove(handle);
				self.events.push_back(Event::StreamClosed(stream));
			}
		}
	}

	#[cfg(feature = "gopher")]
	pub(super) fn progress_fetch(&mut self, now: Instant) {
		let Some(fetch) = &mut self.fetch else {
			return;
		};
		let socket = self.sockets.get_mut::<tcp::Socket>(fetch.socket);
		if socket.may_send() {
			fetch.connected = true;
			if !fetch.sent && socket.send_slice(&fetch.request) == Ok(fetch.request.len()) {
				fetch.sent = true;
			}
		}
		while socket.can_recv() && fetch.response.len() < MAX_FETCH {
			let room = MAX_FETCH - fetch.response.len();
			let _ = socket.recv(|data| {
				let taken = data.len().min(room);
				fetch.response.extend_from_slice(&data[..taken]);
				(taken, ())
			});
		}
		let finished = !socket.is_open() || (fetch.sent && !socket.may_recv());
		let full = fetch.response.len() >= MAX_FETCH;
		let timed_out = now >= fetch.started + FETCH_TIMEOUT;
		if !(finished || full || timed_out) {
			return;
		}
		if finished {
			socket.close();
		} else {
			socket.abort();
		}
		let fetch = self.fetch.take().expect("checked above");
		self.closing.push(fetch.socket);
		let event = if fetch.connected && (finished || full) {
			Event::Fetched {
				data: fetch.response,
			}
		} else {
			Event::FetchFailed
		};
		self.events.push_back(event);
	}
}
