//! The shared folder as an SFTP server sees it: a single directory, with
//! the store's files in it and nothing below.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_io_async::Write;
use log::warn;
use sunset::sshwire::SSHEncode;
use sunset_sftp::error::SftpResult;
use sunset_sftp::protocol::constants::MAX_NAME_ENTRY_SIZE;
use sunset_sftp::protocol::{Attrs, Filename, NameEntry, PFlags, StatusCode};
use sunset_sftp::server::{
	DirHandle, DirReadHeaderReply, DirReadReplyFinished, FileHandle, ReadHeaderReply,
	ReadReplyFinished, ReadStatus, SftpOpResult, SftpServer, SftpSink,
};

use super::{Attendant, Mode, Progress, Store, StoreError, file_name};

/// As many files as a client may have open at once.
const OPEN_FILES: usize = 4;
/// How much of a file is read from the store at a time.
const READ_CHUNK: usize = 512;
/// Permissions shown for the folder and its files, which anyone may read
/// and write.
const FOLDER_MODE: u32 = 0o040_755;
const FILE_MODE: u32 = 0o100_644;

struct OpenFile<F> {
	name: String,
	file: F,
	mode: Mode,
	size: u64,
}

pub(super) struct SharedFiles<'a, S: Store, A: Attendant> {
	store: &'a mut S,
	attendant: &'a A,
	files: [Option<OpenFile<S::File>>; OPEN_FILES],
	/// Whether the folder has been listed since it was opened.
	listed: ReadStatus,
	/// The last path made absolute, which a reply borrows.
	real_path: String,
}

impl<'a, S: Store, A: Attendant> SharedFiles<'a, S, A> {
	pub(super) fn new(store: &'a mut S, attendant: &'a A) -> Self {
		SharedFiles {
			store,
			attendant,
			files: [const { None }; OPEN_FILES],
			listed: ReadStatus::default(),
			real_path: String::new(),
		}
	}
}

fn status(error: StoreError) -> StatusCode {
	match error {
		StoreError::NotFound => StatusCode::SSH_FX_NO_SUCH_FILE,
		StoreError::BadName => StatusCode::SSH_FX_PERMISSION_DENIED,
		StoreError::Full | StoreError::Failed => StatusCode::SSH_FX_FAILURE,
	}
}

fn attrs(size: Option<u64>, mode: u32) -> Attrs {
	Attrs {
		size,
		uid: None,
		gid: None,
		permissions: Some(mode),
		atime: None,
		mtime: None,
		ext_count: None,
	}
}

fn entry<'n>(name: &'n str, size: u64) -> NameEntry<'n> {
	NameEntry {
		filename: Filename::from(name),
		_longname: Filename::from(""),
		attrs: attrs(Some(size), FILE_MODE),
	}
}

impl<S: Store, A: Attendant> SftpServer for SharedFiles<'_, S, A> {
	async fn open(&mut self, path: &str, flags: &PFlags) -> SftpOpResult<FileHandle> {
		let name = file_name(path).map_err(status)?;
		let name = name.ok_or(StatusCode::SSH_FX_FAILURE)?;
		let writing = u32::from(flags) & u32::from(&PFlags::SSH_FXF_WRITE) != 0;
		let mode = if writing { Mode::Write } else { Mode::Read };
		let slot = self
			.files
			.iter()
			.position(Option::is_none)
			.ok_or(StatusCode::SSH_FX_FAILURE)?;
		let size = match mode {
			Mode::Read => self.store.size(name).await.map_err(status)?,
			Mode::Write => 0,
		};
		let file = self.store.open(name, mode).await.map_err(status)?;
		self.files[slot] = Some(OpenFile {
			name: name.to_string(),
			file,
			mode,
			size,
		});
		Ok(FileHandle(slot as u32))
	}

	async fn close(&mut self, handle: FileHandle) -> SftpOpResult<()> {
		let open = self
			.files
			.get_mut(handle.0 as usize)
			.and_then(Option::take)
			.ok_or(StatusCode::SSH_FX_FAILURE)?;
		self.store.close(open.file).await.map_err(status)?;
		let name = open.name.as_str();
		self.attendant.progress(match open.mode {
			Mode::Write => Progress::Received {
				name,
				bytes: open.size,
			},
			Mode::Read => Progress::Read { name },
		});
		Ok(())
	}

	async fn read<W: Write>(
		&mut self,
		handle: FileHandle,
		offset: u64,
		len: u32,
		mut reply: ReadHeaderReply<'_, '_, W>,
	) -> SftpResult<ReadReplyFinished> {
		let store = &mut *self.store;
		let open = self
			.files
			.get_mut(handle.0 as usize)
			.and_then(Option::as_mut)
			.ok_or(StatusCode::SSH_FX_FAILURE)?;
		if offset >= open.size {
			return reply.send_eof().await;
		}
		let length = u64::from(len).min(open.size - offset) as u32;
		let data = reply.send_header(length).await?;
		let file = &mut open.file;
		let finished = data
			.send_data(|mut sender| async move {
				let mut buffer = [0u8; READ_CHUNK];
				let mut at = offset;
				loop {
					if let Some(finished) = sender.completed() {
						return Ok(finished);
					}
					let read = store.read(file, at, &mut buffer).await.map_err(status)?;
					if read == 0 {
						return Err(StatusCode::SSH_FX_FAILURE.into());
					}
					sender.send_data(&buffer[..read]).await?;
					at += read as u64;
				}
			})
			.await?;
		Ok(finished)
	}

	async fn write(&mut self, handle: FileHandle, offset: u64, data: &[u8]) -> SftpOpResult<()> {
		let store = &mut *self.store;
		let open = self
			.files
			.get_mut(handle.0 as usize)
			.and_then(Option::as_mut)
			.ok_or(StatusCode::SSH_FX_FAILURE)?;
		if open.mode != Mode::Write {
			return Err(StatusCode::SSH_FX_PERMISSION_DENIED);
		}
		store
			.write(&mut open.file, offset, data)
			.await
			.map_err(status)?;
		open.size = open.size.max(offset + data.len() as u64);
		self.attendant.progress(Progress::Receiving {
			name: &open.name,
			bytes: open.size,
		});
		Ok(())
	}

	async fn opendir(&mut self, path: &str) -> SftpOpResult<DirHandle> {
		match file_name(path) {
			Ok(None) => {
				self.listed = ReadStatus::default();
				Ok(DirHandle(0))
			}
			_ => Err(StatusCode::SSH_FX_NO_SUCH_FILE),
		}
	}

	async fn closedir(&mut self, _handle: DirHandle) -> SftpOpResult<()> {
		Ok(())
	}

	async fn readdir<W: Write>(
		&mut self,
		_handle: DirHandle,
		mut reply: DirReadHeaderReply<'_, '_, W>,
	) -> SftpOpResult<DirReadReplyFinished> {
		if self.listed == ReadStatus::EndOfFile {
			return reply
				.send_eof()
				.await
				.map_err(|_| StatusCode::SSH_FX_FAILURE);
		}
		self.listed = ReadStatus::EndOfFile;
		let entries = self.store.list().await.map_err(status)?;
		if entries.is_empty() {
			return reply
				.send_eof()
				.await
				.map_err(|_| StatusCode::SSH_FX_FAILURE);
		}
		let mut length = 0;
		for listed in &entries {
			let mut buffer = [0u8; MAX_NAME_ENTRY_SIZE];
			let mut sink = SftpSink::new(&mut buffer);
			entry(&listed.name, listed.size)
				.enc(&mut sink)
				.map_err(|_| StatusCode::SSH_FX_FAILURE)?;
			length += sink.payload_len() as u32;
		}
		let data = reply
			.send_header(length, entries.len() as u32)
			.await
			.map_err(|_| StatusCode::SSH_FX_FAILURE)?;
		let entries: &Vec<_> = &entries;
		data.send_data(|mut sender| async move {
			for listed in entries {
				sender.send_item(&entry(&listed.name, listed.size)).await?;
			}
			sender.completed().ok_or(StatusCode::SSH_FX_FAILURE.into())
		})
		.await
		.map_err(|error| {
			warn!("Listing the shared folder failed: {error:?}");
			StatusCode::SSH_FX_FAILURE
		})
	}

	async fn realpath(&mut self, path: &str) -> SftpOpResult<NameEntry<'_>> {
		self.real_path = match file_name(path).map_err(status)? {
			None => "/".into(),
			Some(name) => alloc::format!("/{name}"),
		};
		Ok(NameEntry {
			filename: Filename::from(self.real_path.as_str()),
			_longname: Filename::from(""),
			attrs: attrs(None, FOLDER_MODE),
		})
	}

	async fn attrs(&mut self, _follow_links: bool, path: &str) -> SftpOpResult<Attrs> {
		match file_name(path).map_err(status)? {
			None => Ok(attrs(None, FOLDER_MODE)),
			Some(name) => {
				let size = self.store.size(name).await.map_err(status)?;
				Ok(attrs(Some(size), FILE_MODE))
			}
		}
	}
}

impl<S: Store, A: Attendant> SharedFiles<'_, S, A> {
	/// Closes whatever the client left open.
	pub(super) async fn close_all(&mut self) {
		for open in self.files.iter_mut().filter_map(Option::take) {
			if let Err(error) = self.store.close(open.file).await {
				warn!("Closing {} failed: {error:?}", open.name);
			}
		}
	}
}
