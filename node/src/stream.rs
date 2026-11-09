//! One Yggdrasil peering over a reliable byte stream: the metadata handshake,
//! then routing traffic handed to petrified.

use alloc::vec::Vec;
use core::fmt;

use petrified::encrypted::PacketConn;
use petrified::types::{Error, PacketConn as _, PeerId, PublicKey};
use yggdrasil_proto::handshake::{HEADER_LEN, HandshakeError, Metadata};

#[derive(Debug)]
pub enum StreamError {
	Handshake(HandshakeError),
	IncompatibleVersion { major: u16, minor: u16 },
	ToSelf,
	Protocol(Error),
}

impl fmt::Display for StreamError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			StreamError::Handshake(error) => write!(f, "handshake failed: {error}"),
			StreamError::IncompatibleVersion { major, minor } => {
				write!(
					f,
					"remote node incompatible version (remote {major}.{minor})"
				)
			}
			StreamError::ToSelf => f.write_str("cannot connect to self"),
			StreamError::Protocol(error) => error.fmt(f),
		}
	}
}

enum State {
	Handshake { received: Vec<u8> },
	Up { peer: PeerId, key: PublicKey },
	Closed,
}

pub(crate) struct Stream {
	state: State,
	/// Our handshake, until it has been written.
	handshake: Vec<u8>,
}

impl Stream {
	pub fn new(handshake: Vec<u8>) -> Self {
		Stream {
			state: State::Handshake {
				received: Vec::new(),
			},
			handshake,
		}
	}

	pub fn key(&self) -> Option<PublicKey> {
		match self.state {
			State::Up { key, .. } => Some(key),
			_ => None,
		}
	}

	pub fn peer(&self) -> Option<PeerId> {
		match self.state {
			State::Up { peer, .. } => Some(peer),
			_ => None,
		}
	}

	pub fn is_handshaking(&self) -> bool {
		matches!(self.state, State::Handshake { .. })
	}

	pub fn is_closed(&self) -> bool {
		matches!(self.state, State::Closed)
	}

	/// Marks the stream closed after petrified dropped its peer.
	pub fn mark_closed(&mut self) {
		self.state = State::Closed;
	}

	pub fn receive(
		&mut self,
		conn: &mut PacketConn,
		password: &[u8],
		data: &[u8],
	) -> Result<(), StreamError> {
		let result = self.receive_inner(conn, password, data);
		if result.is_err() {
			self.close(conn);
		}
		result
	}

	fn receive_inner(
		&mut self,
		conn: &mut PacketConn,
		password: &[u8],
		data: &[u8],
	) -> Result<(), StreamError> {
		match &mut self.state {
			State::Closed => Err(StreamError::Protocol(Error::Closed)),
			State::Up { peer, .. } => conn
				.handle_peer_data(*peer, data)
				.map_err(StreamError::Protocol),
			State::Handshake { received } => {
				received.extend_from_slice(data);
				let Some(header) = received.get(..HEADER_LEN) else {
					return Ok(());
				};
				let header: &[u8; HEADER_LEN] = header.try_into().expect("header length");
				let length = Metadata::body_length(header).map_err(StreamError::Handshake)?;
				let Some(body) = received.get(HEADER_LEN..HEADER_LEN + length) else {
					return Ok(());
				};
				let remote =
					Metadata::decode_body(body, password).map_err(StreamError::Handshake)?;
				let rest = received.split_off(HEADER_LEN + length);
				let key = accept(conn, &remote)?;
				let peer = conn
					.handle_conn(&key, remote.priority)
					.map_err(StreamError::Protocol)?;
				self.state = State::Up { peer, key };
				if rest.is_empty() {
					return Ok(());
				}
				conn.handle_peer_data(peer, &rest)
					.map_err(StreamError::Protocol)
			}
		}
	}

	/// Bytes waiting to be written to the stream.
	pub fn transmit<'a>(&'a self, conn: &'a PacketConn) -> &'a [u8] {
		if !self.handshake.is_empty() {
			return &self.handshake;
		}
		match self.state {
			State::Up { peer, .. } => conn.peer_transmit(peer),
			_ => &[],
		}
	}

	/// Marks the first `n` bytes of [`Stream::transmit`] as written.
	pub fn consume(&mut self, conn: &mut PacketConn, n: usize) {
		if !self.handshake.is_empty() {
			self.handshake.drain(..n.min(self.handshake.len()));
			return;
		}
		if let State::Up { peer, .. } = self.state {
			conn.peer_consume(peer, n);
		}
	}

	pub fn close(&mut self, conn: &mut PacketConn) {
		if let State::Up { peer, .. } = self.state {
			// Already gone if petrified dropped it
			let _ = conn.remove_peer(peer);
		}
		self.state = State::Closed;
		self.handshake.clear();
	}
}

/// Checks the remote handshake the way yggdrasil-go does.
fn accept(conn: &PacketConn, remote: &Metadata) -> Result<PublicKey, StreamError> {
	if !remote.check() {
		return Err(StreamError::IncompatibleVersion {
			major: remote.major_ver,
			minor: remote.minor_ver,
		});
	}
	let key = remote.public_key.expect("checked");
	if key == conn.public_key() {
		return Err(StreamError::ToSelf);
	}
	Ok(key)
}
