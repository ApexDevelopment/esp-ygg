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

use super::pending::PendingWrite;
use super::{Event, Overlay, SendError};

/// Local ports for connections this board opens.
pub(super) const EPHEMERAL_PORTS: core::ops::Range<u16> = 49152..65535;
/// How long a fetch may take, and how much of a response is kept.
#[cfg(feature = "gopher")]
pub(super) const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(feature = "gopher")]
const MAX_FETCH: usize = 32 * 1024;
/// How long a stream may go without an answer before it is given up.
#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
const STREAM_TIMEOUT_SECS: u64 = 300;
/// How long a stream may take to connect.
#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
const CONNECT_TIMEOUT_SECS: u64 = 20;
/// How much may wait to be sent on a stream.
#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
const MAX_STREAM_BACKLOG: usize = 4096;

/// The lasting connections the board can hold, one for each client that
/// needs one, so they can stay open side by side.
#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
	#[cfg(feature = "irc")]
	Irc,
	#[cfg(feature = "gemini")]
	Gemini,
	/// Sending a file to another node.
	#[cfg(feature = "sftp")]
	Sftp,
	/// A peer connected to the board's own SFTP server, which listens
	/// again once it closes.
	#[cfg(feature = "sftp")]
	SftpIncoming,
}

#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
impl Stream {
	/// Whether the host reads the stream itself, with
	/// [`Overlay::stream_receive`], rather than taking events of its data.
	#[cfg(feature = "sftp")]
	fn is_pulled(self) -> bool {
		matches!(self, Stream::Sftp | Stream::SftpIncoming)
	}

	/// Each stream, with its receive buffer size.
	const ALL: &[(Stream, usize)] = &[
		#[cfg(feature = "irc")]
		(Stream::Irc, 2048),
		#[cfg(feature = "gemini")]
		(Stream::Gemini, 4096),
		// Room for an SSH packet
		#[cfg(feature = "sftp")]
		(Stream::Sftp, 2048),
		#[cfg(feature = "sftp")]
		(Stream::SftpIncoming, 2048),
	];
}

#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamState {
	Closed,
	/// Waiting for a peer to connect.
	#[cfg(feature = "sftp")]
	Listening,
	Connecting,
	Open,
}

#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
pub(super) struct StreamSlot {
	stream: Stream,
	receive_buffer: usize,
	/// Made when the stream opens, so a closed stream costs no memory.
	socket: Option<SocketHandle>,
	state: StreamState,
	outgoing: PendingWrite,
}

#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
pub(super) fn stream_slots() -> Vec<StreamSlot> {
	Stream::ALL
		.iter()
		.map(|&(stream, receive_buffer)| StreamSlot {
			stream,
			receive_buffer,
			socket: None,
			state: StreamState::Closed,
			outgoing: PendingWrite::default(),
		})
		.collect()
}

/// A request to another server, and what has come back so far.
#[cfg(feature = "gopher")]
pub(super) struct Fetch {
	socket: SocketHandle,
	request: PendingWrite,
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
			request: PendingWrite::new(request),
			connected: false,
			response: Vec::new(),
			started: now,
		});
		Ok(())
	}

	/// Opens a lasting TCP connection, replacing the one `stream` had open.
	/// Its progress arrives as [`Event`]s.
	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
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
	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
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

	/// Takes up to `max` bytes that came on a stream the host reads itself,
	/// if any did.
	#[cfg(feature = "sftp")]
	pub fn stream_receive(&mut self, stream: Stream, max: usize) -> Option<Vec<u8>> {
		let handle = self.slot(stream).socket?;
		let socket = self.sockets.get_mut::<tcp::Socket>(handle);
		let mut data = Vec::new();
		let _ = socket.recv(|bytes| {
			let taken = bytes.len().min(max);
			data.extend_from_slice(&bytes[..taken]);
			(taken, ())
		});
		(!data.is_empty()).then_some(data)
	}

	/// How much is waiting to be sent on a stream.
	#[cfg(feature = "sftp")]
	pub fn stream_backlog(&mut self, stream: Stream) -> usize {
		let slot = self.slot(stream);
		let (queued, socket) = (slot.outgoing.len(), slot.socket);
		let in_socket = socket.map_or(0, |handle| {
			self.sockets.get::<tcp::Socket>(handle).send_queue()
		});
		queued + in_socket
	}

	/// Closes a stream once what is queued has been sent.
	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
	pub fn close_stream(&mut self, stream: Stream) {
		if let Some(socket) = self.slot(stream).socket {
			self.sockets.get_mut::<tcp::Socket>(socket).close();
		}
	}

	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
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

	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
	pub(super) fn progress_streams(&mut self) {
		#[cfg(feature = "sftp")]
		let mut listen_again = false;
		for slot in &mut self.streams {
			let Some(handle) = slot.socket else {
				continue;
			};
			let stream = slot.stream;
			let socket = self.sockets.get_mut::<tcp::Socket>(handle);
			#[cfg(feature = "sftp")]
			if slot.state == StreamState::Listening {
				if !socket.may_send() {
					continue;
				}
				slot.state = StreamState::Open;
				socket.set_timeout(Some(smoltcp::time::Duration::from_secs(
					STREAM_TIMEOUT_SECS,
				)));
				if let Some(peer) = socket.remote_endpoint()
					&& let IpAddress::Ipv6(from) = peer.addr
				{
					self.events
						.push_back(Event::StreamAccepted { stream, from });
				}
			}
			if slot.state == StreamState::Connecting && socket.may_send() {
				slot.state = StreamState::Open;
				socket.set_timeout(Some(smoltcp::time::Duration::from_secs(
					STREAM_TIMEOUT_SECS,
				)));
				self.events.push_back(Event::StreamOpened(stream));
			}
			if slot.state == StreamState::Open && slot.outgoing.write_to(socket) {
				slot.outgoing.clear();
			}
			// The host reads SFTP's streams itself, as fast as it can take
			// them, so the sender slows to match.
			#[cfg(feature = "sftp")]
			let pulled = stream.is_pulled();
			#[cfg(not(feature = "sftp"))]
			let pulled = false;
			while !pulled && socket.can_recv() {
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
			if !socket.is_open() && !(pulled && socket.recv_queue() > 0) {
				slot.state = StreamState::Closed;
				slot.socket = None;
				slot.outgoing.clear();
				self.sockets.remove(handle);
				self.events.push_back(Event::StreamClosed(stream));
				#[cfg(feature = "sftp")]
				if stream == Stream::SftpIncoming {
					listen_again = true;
				}
			}
		}
		#[cfg(feature = "sftp")]
		if listen_again {
			self.listen_for_sftp();
		}
	}

	/// Waits for a peer to connect to the board's SFTP server.
	#[cfg(feature = "sftp")]
	pub(super) fn listen_for_sftp(&mut self) {
		let slot = self.slot(Stream::SftpIncoming);
		let receive_buffer = slot.receive_buffer;
		let mut socket = tcp::Socket::new(
			tcp::SocketBuffer::new(vec![0; receive_buffer]),
			tcp::SocketBuffer::new(vec![0; 1024]),
		);
		if socket.listen(crate::sftp::PORT).is_err() {
			return;
		}
		let handle = self.sockets.add(socket);
		let slot = self.slot(Stream::SftpIncoming);
		slot.socket = Some(handle);
		slot.state = StreamState::Listening;
	}

	#[cfg(feature = "gopher")]
	pub(super) fn progress_fetch(&mut self, now: Instant) {
		let Some(fetch) = &mut self.fetch else {
			return;
		};
		let socket = self.sockets.get_mut::<tcp::Socket>(fetch.socket);
		if socket.may_send() {
			fetch.connected = true;
			fetch.request.write_to(socket);
		}
		while socket.can_recv() && fetch.response.len() < MAX_FETCH {
			let room = MAX_FETCH - fetch.response.len();
			let _ = socket.recv(|data| {
				let taken = data.len().min(room);
				fetch.response.extend_from_slice(&data[..taken]);
				(taken, ())
			});
		}
		let finished = !socket.is_open() || (fetch.request.is_empty() && !socket.may_recv());
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
