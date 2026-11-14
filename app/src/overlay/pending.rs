use alloc::vec::Vec;
use smoltcp::socket::tcp;

#[derive(Default)]
pub(super) struct PendingWrite {
	bytes: Vec<u8>,
	sent: usize,
}

impl PendingWrite {
	#[cfg(any(feature = "http", feature = "gopher"))]
	pub fn new(bytes: Vec<u8>) -> Self {
		Self { bytes, sent: 0 }
	}

	pub fn len(&self) -> usize {
		self.bytes.len() - self.sent
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
	pub fn clear(&mut self) {
		*self = Self::default();
	}

	#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
	pub fn extend_from_slice(&mut self, bytes: &[u8]) {
		self.bytes.drain(..self.sent);
		self.sent = 0;
		self.bytes.extend_from_slice(bytes);
	}

	pub fn write_to(&mut self, socket: &mut tcp::Socket<'_>) -> bool {
		if !self.is_empty()
			&& let Ok(sent) = socket.send_slice(&self.bytes[self.sent..])
		{
			self.sent += sent;
		}
		self.is_empty()
	}
}
