//! Sending a file to any SFTP server over a connection already open: SSH
//! to log in, then just enough of SFTP version 3 to write the file.

use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use embassy_futures::select::{Either3, select3};
use embassy_sync::signal::Signal;
use embedded_io_async::{Read, Write};
use log::info;
use sunset::{CliEvent, PubKey};
use sunset_async::{ChanInOut, ProgressHolder, SSHClient, SunsetRawMutex};
use yggdrasil_espnow::PublicKey;
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

use super::{Attendant, Mode, Progress, Store, StoreError};

const SSH_BUFFER: usize = 2048;
/// How much of the file goes in each write, which every server takes.
const CHUNK: usize = 1024;

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXF_WRITE: u32 = 0x02;
const SSH_FXF_CREAT: u32 = 0x08;
const SSH_FXF_TRUNC: u32 = 0x10;
const SSH_FX_OK: u32 = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
	/// The server's key is a board's, but not the one at the address.
	WrongKey,
	/// The server would not let us in.
	Refused,
	/// The server would not take the file, saying why.
	Rejected(String),
	Store(StoreError),
	/// The connection broke, or the server spoke nonsense.
	Failed,
}

impl From<sunset::Error> for SendError {
	fn from(_: sunset::Error) -> Self {
		SendError::Failed
	}
}

impl From<StoreError> for SendError {
	fn from(error: StoreError) -> Self {
		SendError::Store(error)
	}
}

/// Sends the file `name` from the store to the server at `to`, logging in
/// as `user`, over a connection already open. A server with an ed25519 key
/// that makes a Yggdrasil address is taken to be a board, and must be the
/// one at `to`.
#[allow(clippy::too_many_arguments)]
pub async fn send<S: Store, A: Attendant>(
	input: &mut impl Read,
	output: &mut impl Write,
	to: Ipv6Addr,
	user: &str,
	name: &str,
	store: &mut S,
	attendant: &A,
) -> Result<(), SendError> {
	let mut inbuf = [0u8; SSH_BUFFER];
	let mut outbuf = [0u8; SSH_BUFFER];
	let client = SSHClient::new(&mut inbuf, &mut outbuf);
	let session = Signal::<SunsetRawMutex, ChanInOut<'_>>::new();
	let mut wrong_key = false;
	let mut refused = true;

	let events = async {
		let mut opening = None;
		loop {
			let mut holder = ProgressHolder::new();
			match client.progress(&mut holder).await? {
				CliEvent::Hostkey(check) => {
					if is_other_board(&check.hostkey()?, to) {
						wrong_key = true;
						check.reject()?;
					} else {
						check.accept()?;
					}
				}
				CliEvent::Username(request) => request.username(user)?,
				CliEvent::Password(request) => match attendant.password(user).await {
					Some(password) => request.password(password)?,
					None => request.skip()?,
				},
				CliEvent::Pubkey(request) => request.skip()?,
				CliEvent::Authenticated => {
					refused = false;
					// The holder locks the session, which opening it needs.
					drop(holder);
					let (channel, _) = client.open_session_nopty().await?;
					opening = Some(channel);
				}
				// Writing before the server opens the channel would fail.
				CliEvent::SessionOpened(mut opener) => {
					opener.subsystem("sftp")?;
					if let Some(channel) = opening.take() {
						session.signal(channel);
					}
				}
				CliEvent::Defunct => return Err(SendError::Failed),
				CliEvent::Banner(_)
				| CliEvent::AgentSign(_)
				| CliEvent::SessionExit(_)
				| CliEvent::PollAgain => {}
			}
		}
	};

	let transfer = async {
		let mut channel = session.wait().await;
		upload(&mut channel, name, store, attendant).await
	};

	let result = match select3(client.run(input, output), events, transfer).await {
		Either3::First(result) => result.map_err(SendError::from),
		Either3::Second(result) => result,
		Either3::Third(result) => result,
	};
	match result {
		Err(_) if wrong_key => Err(SendError::WrongKey),
		Err(SendError::Failed) if refused => Err(SendError::Refused),
		result => result,
	}
}

/// Whether a host key makes a Yggdrasil address other than `to`'s, so is a
/// board's but not the one dialled.
fn is_other_board(key: &PubKey<'_>, to: Ipv6Addr) -> bool {
	let PubKey::Ed25519(key) = key else {
		return false;
	};
	let key = PublicKey(key.key.0);
	let address = Ipv6Addr::from(addr_for_key(&key).0);
	is_yggdrasil(address) && address != to && is_yggdrasil(to)
}

/// Whether an address is in Yggdrasil's range, 200::/7.
fn is_yggdrasil(address: Ipv6Addr) -> bool {
	address.octets()[0] & 0xfe == 0x02
}

/// Writes the file over an SFTP channel: open, write, close.
async fn upload<S: Store, A: Attendant>(
	channel: &mut ChanInOut<'_>,
	name: &str,
	store: &mut S,
	attendant: &A,
) -> Result<(), SendError> {
	let total = store.size(name)?;
	let mut file = store.open(name, Mode::Read)?;
	let result = write_file(channel, name, total, store, &mut file, attendant).await;
	store.close(file)?;
	result?;
	attendant.progress(Progress::Sent { name });
	info!("Sent {name}");
	Ok(())
}

async fn write_file<S: Store, A: Attendant>(
	channel: &mut ChanInOut<'_>,
	name: &str,
	total: u64,
	store: &mut S,
	file: &mut S::File,
	attendant: &A,
) -> Result<(), SendError> {
	let mut packet = Packet::new(SSH_FXP_INIT);
	packet.u32(3);
	exchange(channel, &packet, SSH_FXP_VERSION).await?;

	let mut packet = Packet::request(SSH_FXP_OPEN, 1);
	packet.string(name.as_bytes());
	packet.u32(SSH_FXF_WRITE | SSH_FXF_CREAT | SSH_FXF_TRUNC);
	// No attributes
	packet.u32(0);
	let reply = exchange(channel, &packet, SSH_FXP_HANDLE).await?;
	let handle = Reader(&reply[4..]).string()?.to_vec();

	let mut buffer = [0u8; CHUNK];
	let mut offset = 0;
	let mut id = 2;
	loop {
		let read = store.read(file, offset, &mut buffer)?;
		if read == 0 {
			break;
		}
		let mut packet = Packet::request(SSH_FXP_WRITE, id);
		packet.string(&handle);
		packet.u64(offset);
		packet.string(&buffer[..read]);
		expect_ok(exchange(channel, &packet, SSH_FXP_STATUS).await?)?;
		offset += read as u64;
		id += 1;
		attendant.progress(Progress::Sending {
			name,
			bytes: offset,
			total,
		});
	}

	let mut packet = Packet::request(SSH_FXP_CLOSE, id);
	packet.string(&handle);
	expect_ok(exchange(channel, &packet, SSH_FXP_STATUS).await?)
}

/// Sends a packet and reads the answer, which must be of type `expected`
/// or a status. Returns the answer's body after its type.
async fn exchange(
	channel: &mut ChanInOut<'_>,
	packet: &Packet,
	expected: u8,
) -> Result<Vec<u8>, SendError> {
	channel
		.write_all(&packet.finish())
		.await
		.map_err(|_| SendError::Failed)?;
	let mut length = [0u8; 4];
	channel
		.read_exact(&mut length)
		.await
		.map_err(|_| SendError::Failed)?;
	let length = u32::from_be_bytes(length) as usize;
	// A status, or a handle, is all that comes back here.
	if length == 0 || length > 1024 {
		return Err(SendError::Failed);
	}
	let mut body = alloc::vec![0u8; length];
	channel
		.read_exact(&mut body)
		.await
		.map_err(|_| SendError::Failed)?;
	match body[0] {
		kind if kind == expected => Ok(body[1..].to_vec()),
		SSH_FXP_STATUS => {
			expect_ok(body[1..].to_vec())?;
			Err(SendError::Failed)
		}
		_ => Err(SendError::Failed),
	}
}

/// Checks a status's body, after its request id, for success.
fn expect_ok(body: Vec<u8>) -> Result<(), SendError> {
	let mut reader = Reader(body.get(4..).ok_or(SendError::Failed)?);
	match reader.u32()? {
		SSH_FX_OK => Ok(()),
		_ => {
			let message = reader.string().unwrap_or_default();
			Err(SendError::Rejected(String::from_utf8_lossy(message).into()))
		}
	}
}

/// An SFTP packet being built, its length written last.
struct Packet(Vec<u8>);

impl Packet {
	fn new(kind: u8) -> Self {
		Packet(alloc::vec![0, 0, 0, 0, kind])
	}

	fn request(kind: u8, id: u32) -> Self {
		let mut packet = Packet::new(kind);
		packet.u32(id);
		packet
	}

	fn u32(&mut self, value: u32) {
		self.0.extend_from_slice(&value.to_be_bytes());
	}

	fn u64(&mut self, value: u64) {
		self.0.extend_from_slice(&value.to_be_bytes());
	}

	fn string(&mut self, value: &[u8]) {
		self.u32(value.len() as u32);
		self.0.extend_from_slice(value);
	}

	fn finish(&self) -> Vec<u8> {
		let mut bytes = self.0.clone();
		let length = (bytes.len() - 4) as u32;
		bytes[..4].copy_from_slice(&length.to_be_bytes());
		bytes
	}
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
	fn u32(&mut self) -> Result<u32, SendError> {
		let (bytes, rest) = self.0.split_at_checked(4).ok_or(SendError::Failed)?;
		self.0 = rest;
		Ok(u32::from_be_bytes(bytes.try_into().unwrap_or_default()))
	}

	fn string(&mut self) -> Result<&'a [u8], SendError> {
		let length = self.u32()? as usize;
		let (bytes, rest) = self.0.split_at_checked(length).ok_or(SendError::Failed)?;
		self.0 = rest;
		Ok(bytes)
	}
}
