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

use alloc::format;
use alloc::string::String;
use core::net::Ipv6Addr;

pub use client::{SendError, send};
pub use server::serve;

/// The port Share serves on, SSH's own.
pub const PORT: u16 = 22;

/// The folder that is shared, holding files by name alone. It is async,
/// as a board's card may be busy with the screen.
#[allow(async_fn_in_trait)]
pub trait Store {
	type File;

	/// The files in the folder.
	async fn list(&mut self) -> Result<alloc::vec::Vec<Entry>, StoreError>;

	async fn size(&mut self, name: &str) -> Result<u64, StoreError>;

	/// Opens a file to read, or to write afresh, emptying it if it exists.
	async fn open(&mut self, name: &str, mode: Mode) -> Result<Self::File, StoreError>;

	async fn read(
		&mut self,
		file: &mut Self::File,
		offset: u64,
		buf: &mut [u8],
	) -> Result<usize, StoreError>;

	async fn write(
		&mut self,
		file: &mut Self::File,
		offset: u64,
		data: &[u8],
	) -> Result<(), StoreError>;

	async fn close(&mut self, file: Self::File) -> Result<(), StoreError>;
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

/// What Share is doing, for the interface to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
	/// A peer waiting to be let in, until the person answers.
	pub asking: Option<Ipv6Addr>,
	/// The user a server wants a password for, until the person gives one.
	pub password_for: Option<String>,
	/// The file going in or out.
	pub transfer: Option<Transfer>,
	/// How the last transfer ended, such as "Received notes.txt".
	pub last: Option<String>,
}

impl Status {
	/// What Share is doing or last did, in a line.
	pub fn summary(&self) -> String {
		match (&self.transfer, &self.last) {
			(Some(transfer), _) if transfer.incoming => {
				format!(
					"Receiving {} · {}",
					transfer.name,
					size_text(transfer.bytes)
				)
			}
			(Some(transfer), _) => format!(
				"Sending {} · {} of {}",
				transfer.name,
				size_text(transfer.bytes),
				size_text(transfer.total.unwrap_or_default())
			),
			(None, Some(last)) => last.clone(),
			(None, None) => String::new(),
		}
	}

	/// Whether a transfer ended since `before`, so the folder is worth
	/// listing again. A sent file counts too, as sending to this board
	/// itself ends with "Sent" right after "Received".
	pub fn ended_since(&self, before: &Status) -> bool {
		self.last != before.last && self.last.is_some()
	}
}

/// A size as people write it.
pub fn size_text(bytes: u64) -> String {
	match bytes {
		0..1024 => format!("{bytes} B"),
		1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
		_ => format!(
			"{}.{} MB",
			bytes / 1_048_576,
			bytes % 1_048_576 * 10 / 1_048_576
		),
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
	pub name: String,
	pub bytes: u64,
	/// How big the file is, when sending, where it is known.
	pub total: Option<u64>,
	pub incoming: bool,
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

/// The short name a FAT card can hold for a file, as Windows makes them:
/// `notes.txt` stays `NOTES.TXT`, while `holiday photo.jpeg` becomes
/// `HOLIDA~1.JPE`, with `number` telling apart others that shorten alike.
/// A `number` above 0 numbers even a name that fits, as `NOTES~1.TXT`.
pub fn short_name(name: &str, number: u32) -> String {
	const ALLOWED: &str = "!#$%&'()-@^_`{}~";
	let keep = |part: &str| -> String {
		part.chars()
			.filter(|c| c.is_ascii_alphanumeric() || ALLOWED.contains(*c))
			.map(|c| c.to_ascii_uppercase())
			.collect()
	};
	let (base, extension) = match name.rsplit_once('.') {
		Some((base, extension)) if !base.is_empty() => (base, extension),
		_ => (name, ""),
	};
	let (short_base, short_extension) = (keep(base), keep(extension));
	let fits = number == 0
		&& short_base.len() <= 8
		&& short_extension.len() <= 3
		&& !short_base.is_empty()
		&& short_base.eq_ignore_ascii_case(base)
		&& short_extension.eq_ignore_ascii_case(extension);
	let mut short = if fits {
		short_base
	} else {
		let base = if short_base.is_empty() {
			"FILE".into()
		} else {
			short_base
		};
		let suffix = alloc::format!("~{}", number.max(1));
		let room = 8 - suffix.len();
		alloc::format!("{}{suffix}", &base[..base.len().min(room)])
	};
	if !short_extension.is_empty() {
		short.push('.');
		short.push_str(&short_extension[..short_extension.len().min(3)]);
	}
	short
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
	fn shortens_names_as_windows_does() {
		assert_eq!(short_name("notes.txt", 0), "NOTES.TXT");
		assert_eq!(short_name("notes.txt", 1), "NOTES~1.TXT");
		assert_eq!(short_name("holiday photo.jpeg", 0), "HOLIDA~1.JPE");
		assert_eq!(short_name("holiday photo.jpeg", 2), "HOLIDA~2.JPE");
		assert_eq!(short_name("README", 0), "README");
		assert_eq!(short_name("archive.tar.gz", 0), "ARCHIV~1.GZ");
		assert_eq!(short_name("ünïcode.txt", 0), "NCODE~1.TXT");
		assert_eq!(short_name(".hidden", 0), "HIDDEN~1");
	}

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
