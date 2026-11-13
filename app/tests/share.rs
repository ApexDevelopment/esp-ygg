//! Runs Share's server against OpenSSH's `sftp`, and its sender against
//! the server, over local TCP, with a folder on disk as the shared one.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{Read as _, Seek, SeekFrom, Write as _};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use embedded_io_adapters::tokio_1::FromTokio;
use esp_ygg_app::share::{self, Attendant, Entry, Mode, Progress, SendError, Store, StoreError};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

const SEED: [u8; 32] = [7; 32];

/// A folder on disk.
struct Folder(PathBuf);

impl Folder {
	fn new(name: &str) -> Self {
		let path =
			std::env::temp_dir().join(format!("esp-ygg-share-{name}-{}", std::process::id()));
		let _ = fs::remove_dir_all(&path);
		fs::create_dir_all(&path).unwrap();
		Folder(path)
	}

	fn path(&self, name: &str) -> PathBuf {
		self.0.join(name)
	}
}

impl Drop for Folder {
	fn drop(&mut self) {
		let _ = fs::remove_dir_all(&self.0);
	}
}

impl Store for Folder {
	type File = fs::File;

	async fn list(&mut self) -> Result<Vec<Entry>, StoreError> {
		let mut entries: Vec<Entry> = fs::read_dir(&self.0)
			.map_err(|_| StoreError::Failed)?
			.filter_map(Result::ok)
			.map(|entry| Entry {
				name: entry.file_name().to_string_lossy().into(),
				size: entry.metadata().map(|m| m.len()).unwrap_or_default(),
			})
			.collect();
		entries.sort_by(|a, b| a.name.cmp(&b.name));
		Ok(entries)
	}

	async fn size(&mut self, name: &str) -> Result<u64, StoreError> {
		let metadata = fs::metadata(self.path(name)).map_err(|_| StoreError::NotFound)?;
		Ok(metadata.len())
	}

	async fn open(&mut self, name: &str, mode: Mode) -> Result<fs::File, StoreError> {
		let path = self.path(name);
		match mode {
			Mode::Read => fs::File::open(path).map_err(|_| StoreError::NotFound),
			Mode::Write => fs::File::create(path).map_err(|_| StoreError::Failed),
		}
	}

	async fn read(
		&mut self,
		file: &mut fs::File,
		offset: u64,
		buf: &mut [u8],
	) -> Result<usize, StoreError> {
		file.seek(SeekFrom::Start(offset))
			.map_err(|_| StoreError::Failed)?;
		file.read(buf).map_err(|_| StoreError::Failed)
	}

	async fn write(
		&mut self,
		file: &mut fs::File,
		offset: u64,
		data: &[u8],
	) -> Result<(), StoreError> {
		file.seek(SeekFrom::Start(offset))
			.map_err(|_| StoreError::Failed)?;
		file.write_all(data).map_err(|_| StoreError::Failed)
	}

	async fn close(&mut self, _file: fs::File) -> Result<(), StoreError> {
		Ok(())
	}
}

/// Answers as told, and remembers what it was asked and told.
#[derive(Default)]
struct Person {
	admits: bool,
	password: Option<String>,
	asked: Cell<usize>,
	received: RefCell<Vec<(String, u64)>>,
	sent: RefCell<Vec<String>>,
}

impl Attendant for Person {
	async fn admit(&self, _from: Ipv6Addr) -> bool {
		self.asked.set(self.asked.get() + 1);
		self.admits
	}

	async fn password(&self, _user: &str) -> Option<String> {
		self.password.clone()
	}

	fn progress(&self, progress: Progress<'_>) {
		match progress {
			Progress::Received { name, bytes } => {
				self.received.borrow_mut().push((name.into(), bytes))
			}
			Progress::Sent { name } => self.sent.borrow_mut().push(name.into()),
			_ => {}
		}
	}
}

/// Serves one connection on a local port, while `client` runs against it.
async fn serve_one<F: std::future::Future>(
	folder: &mut Folder,
	password: Option<&str>,
	person: &Person,
	client: impl FnOnce(u16) -> F,
) -> F::Output {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let port = listener.local_addr().unwrap().port();
	let server = async {
		let (stream, _) = listener.accept().await.unwrap();
		let (input, output) = stream.into_split();
		let (mut input, mut output) = (FromTokio::new(input), FromTokio::new(output));
		let from = Ipv6Addr::LOCALHOST;
		let _ = share::serve(
			&mut input,
			&mut output,
			from,
			&SEED,
			password,
			folder,
			person,
		)
		.await;
	};
	let (_, output) = tokio::join!(server, client(port));
	output
}

/// Runs `sftp` with commands, giving `password` when asked, and returns
/// whether it succeeded.
async fn sftp(port: u16, commands: &str, password: Option<&str>, dir: &Path) -> bool {
	let batch = dir.join("batch");
	fs::write(&batch, commands).unwrap();
	let askpass = dir.join("askpass");
	fs::write(
		&askpass,
		format!("#!/bin/sh\necho '{}'\n", password.unwrap_or("")),
	)
	.unwrap();
	fs::set_permissions(
		&askpass,
		std::os::unix::fs::PermissionsExt::from_mode(0o755),
	)
	.unwrap();
	let status = Command::new("sftp")
		.current_dir(dir)
		.args([
			"-o",
			"StrictHostKeyChecking=no",
			"-o",
			"UserKnownHostsFile=/dev/null",
		])
		.args([
			"-o",
			"PubkeyAuthentication=no",
			"-o",
			"NumberOfPasswordPrompts=1",
		])
		.args(["-o", "LogLevel=ERROR", "-o", "BatchMode=no"])
		.args(["-P", &port.to_string(), "-b"])
		.arg(&batch)
		.arg("127.0.0.1")
		.env("SSH_ASKPASS", &askpass)
		.env("SSH_ASKPASS_REQUIRE", "force")
		.stdin(Stdio::null())
		.status()
		.await
		.unwrap();
	status.success()
}

#[tokio::test(flavor = "current_thread")]
async fn takes_and_gives_files_to_openssh() {
	let mut shared = Folder::new("served");
	fs::write(shared.path("hello.txt"), "hello from the board\n").unwrap();
	let local = Folder::new("client");
	let upload: Vec<u8> = (0..20_000u32).flat_map(|n| n.to_le_bytes()).collect();
	fs::write(local.path("big.bin"), &upload).unwrap();
	let person = Person {
		admits: true,
		..Person::default()
	};
	let commands = "put big.bin\nls -l\nget hello.txt fetched.txt\n";
	let worked = serve_one(&mut shared, None, &person, |port| {
		sftp(port, commands, None, &local.0)
	})
	.await;
	assert!(worked, "sftp failed");
	assert_eq!(fs::read(shared.path("big.bin")).unwrap(), upload);
	assert_eq!(
		fs::read_to_string(local.path("fetched.txt")).unwrap(),
		"hello from the board\n"
	);
	assert_eq!(person.asked.get(), 1);
	assert_eq!(
		*person.received.borrow(),
		[("big.bin".to_string(), upload.len() as u64)]
	);
}

#[tokio::test(flavor = "current_thread")]
async fn lets_the_password_straight_in() {
	let mut shared = Folder::new("password");
	let local = Folder::new("password-client");
	fs::write(local.path("note.txt"), "note").unwrap();
	let person = Person::default();
	let worked = serve_one(&mut shared, Some("hunter22"), &person, |port| {
		sftp(port, "put note.txt\n", Some("hunter22"), &local.0)
	})
	.await;
	assert!(worked, "sftp failed");
	assert_eq!(person.asked.get(), 0);
	assert_eq!(fs::read_to_string(shared.path("note.txt")).unwrap(), "note");
}

#[tokio::test(flavor = "current_thread")]
async fn asks_about_a_wrong_password() {
	let mut shared = Folder::new("wrong");
	let local = Folder::new("wrong-client");
	fs::write(local.path("note.txt"), "note").unwrap();
	let person = Person::default();
	let worked = serve_one(&mut shared, Some("hunter22"), &person, |port| {
		sftp(port, "put note.txt\n", Some("guess"), &local.0)
	})
	.await;
	assert!(!worked, "a declined peer got in");
	assert_eq!(person.asked.get(), 1);
	assert!(!shared.path("note.txt").exists());
}

/// Sends a file from one folder to a board serving another.
async fn send_to(
	receiving: &mut Folder,
	sending: &mut Folder,
	name: &str,
) -> Result<(), SendError> {
	let board = Person {
		admits: true,
		..Person::default()
	};
	let sender = Person::default();
	serve_one(receiving, None, &board, |port| async move {
		let stream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port)))
			.await
			.unwrap();
		let (input, output) = stream.into_split();
		let (mut input, mut output) = (FromTokio::new(input), FromTokio::new(output));
		share::send(&mut input, &mut output, "esp-ygg", name, sending, &sender).await
	})
	.await
}

#[tokio::test(flavor = "current_thread")]
async fn sends_to_a_board() {
	let mut receiving = Folder::new("receiving");
	let mut sending = Folder::new("sending");
	let photo: Vec<u8> = (0..5000u32).map(|n| n as u8).collect();
	fs::write(sending.path("photo.jpg"), &photo).unwrap();
	send_to(&mut receiving, &mut sending, "photo.jpg")
		.await
		.unwrap();
	assert_eq!(fs::read(receiving.path("photo.jpg")).unwrap(), photo);
}
