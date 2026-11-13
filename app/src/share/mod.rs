//! Share: files sent and received over SFTP, so any SFTP software can trade
//! files with a board. A board serves one folder, which peers can list,
//! download from and upload to, and sends its files to any SFTP server.
//!
//! The board's SSH host key is its Yggdrasil key, so a board sending to
//! another can check that the key matches the address it dialled.
//!
//! What runs here is the protocol alone. The host carries the connection's
//! bytes, keeps the files in a [`Store`], and asks the person at the board
//! through an [`Attendant`].

mod client;
mod files;
mod server;

use alloc::string::String;
use core::net::Ipv6Addr;

pub use client::{SendError, send};
pub use server::serve;

/// The port Share serves on, SSH's own.
pub const PORT: u16 = 22;

/// The folder that is shared, holding files by name alone.
pub trait Store {
	type File;

	/// The files in the folder.
	fn list(&mut self) -> Result<alloc::vec::Vec<Entry>, StoreError>;

	fn size(&mut self, name: &str) -> Result<u64, StoreError>;

	/// Opens a file to read, or to write afresh, emptying it if it exists.
	fn open(&mut self, name: &str, mode: Mode) -> Result<Self::File, StoreError>;

	fn read(
		&mut self,
		file: &mut Self::File,
		offset: u64,
		buf: &mut [u8],
	) -> Result<usize, StoreError>;

	fn write(&mut self, file: &mut Self::File, offset: u64, data: &[u8]) -> Result<(), StoreError>;

	fn close(&mut self, file: Self::File) -> Result<(), StoreError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
	pub name: String,
	pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	Read,
	Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
	NotFound,
	/// The name will not do, such as a path rather than a file name.
	BadName,
	Full,
	Failed,
}

/// The person at the board, as far as Share needs them. Share runs on one
/// thread, so its futures need not be `Send`.
#[allow(async_fn_in_trait)]
pub trait Attendant {
	/// Whether to let in someone who gave no password, or a wrong one.
	async fn admit(&self, from: Ipv6Addr) -> bool;

	/// The password for the server being sent to, if the person gives one.
	async fn password(&self, user: &str) -> Option<String>;

	/// How the files are going.
	fn progress(&self, progress: Progress<'_>);
}

/// What has happened to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress<'a> {
	/// Some of a file has come in, `bytes` of it so far.
	Receiving {
		name: &'a str,
		bytes: u64,
	},
	/// A file came in whole.
	Received {
		name: &'a str,
		bytes: u64,
	},
	/// A peer has read part of a file.
	Read {
		name: &'a str,
	},
	/// Some of a file has gone, out of `total`.
	Sending {
		name: &'a str,
		bytes: u64,
		total: u64,
	},
	Sent {
		name: &'a str,
	},
}

/// The file name in a path from a client: `name`, `/name` or `./name` are
/// the same file, and the folder itself is `None`. Anything deeper, or
/// reaching outside it, will not do.
pub(crate) fn file_name(path: &str) -> Result<Option<&str>, StoreError> {
	let name = path.trim_start_matches("./").trim_start_matches('/');
	match name {
		"" | "." => Ok(None),
		".." => Err(StoreError::BadName),
		name if name.contains('/') => Err(StoreError::BadName),
		name => Ok(Some(name)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keeps_to_the_folder() {
		assert_eq!(file_name("/"), Ok(None));
		assert_eq!(file_name("."), Ok(None));
		assert_eq!(file_name("/notes.txt"), Ok(Some("notes.txt")));
		assert_eq!(file_name("./notes.txt"), Ok(Some("notes.txt")));
		assert_eq!(file_name("/../etc/passwd"), Err(StoreError::BadName));
		assert_eq!(file_name("sub/notes.txt"), Err(StoreError::BadName));
		assert_eq!(file_name(".."), Err(StoreError::BadName));
	}
}
