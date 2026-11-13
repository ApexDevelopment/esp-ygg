//! Share on the board. Two tasks on the UI's side run SSH sessions, one
//! serving peers and one sending, so the board can do both at once, and
//! even send to itself. The network loop carries the sessions' bytes, reading them
//! only as fast as the task takes them so TCP slows the sender to match.
//! The UI's loop keeps the shared folder, as only it may use the SD card,
//! which on the touch board shares its bus with the screen.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::net::{Ipv6Addr, SocketAddrV6};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::CriticalSectionMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use esp_ygg_app::share::{
	self, Attendant, Entry, Mode, Progress, SendError, Status, Store, StoreError, Transfer,
};
use esp_ygg_app::ui::Command;
use esp_ygg_app::{Event, Overlay, Stream};
use log::{info, warn};

use crate::ui::FROM_UI;

/// How long the board waits for the person to answer a question.
const ANSWER_TIME: Duration = Duration::from_secs(60);
/// How long a send waits to connect, which the network gives up sooner.
const CONNECT_TIME: Duration = Duration::from_secs(30);
/// How much of a stream goes in each piece handed over.
const PIECE: usize = 1024;
/// How much may wait to be sent on a stream, below the network's limit.
const MOST_BACKLOG: usize = 3072;
/// How much of a file is read ahead, or gathered before writing, so the
/// card is woken less often.
const CARD_CHUNK: usize = 4096;

/// What happens on one of Share's streams.
pub enum Wire {
	/// A peer connected to the board's server.
	Accepted(Ipv6Addr),
	/// The board's connection to a server opened.
	Opened,
	Data(Vec<u8>),
	Closed,
}

type Pipe = Channel<CriticalSectionRawMutex, Wire, 4>;

/// Peers' connections to the board, and the board's to servers.
static INCOMING: Pipe = Channel::new();
static OUTGOING: Pipe = Channel::new();
/// How much is waiting to be sent on each, as the network last saw it.
static INCOMING_BACKLOG: AtomicUsize = AtomicUsize::new(0);
static OUTGOING_BACKLOG: AtomicUsize = AtomicUsize::new(0);
/// Wakes the network loop once the task makes room for more.
pub static ROOM: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The node's private key seed, which is also its SSH host key.
static SEED: CriticalSectionMutex<Cell<[u8; 32]>> = CriticalSectionMutex::new(Cell::new([0; 32]));

/// Sends asked for, the person's answers, and the password to let peers in.
static SENDS: Channel<CriticalSectionRawMutex, (Ipv6Addr, String, String), 2> = Channel::new();
static ADMIT: Signal<CriticalSectionRawMutex, bool> = Signal::new();
static PASSWORD: Signal<CriticalSectionRawMutex, Option<String>> = Signal::new();
static SERVER_PASSWORD: CriticalSectionMutex<RefCell<Option<String>>> =
	CriticalSectionMutex::new(RefCell::new(None));

/// What Share is doing, for the interface, and whether that changed.
static STATUS: CriticalSectionMutex<RefCell<Status>> =
	CriticalSectionMutex::new(RefCell::new(Status {
		asking: None,
		password_for: None,
		transfer: None,
		last: None,
	}));
static STATUS_CHANGED: AtomicBool = AtomicBool::new(false);

/// Sets the host key, and the password that lets peers straight in.
pub fn set_up(seed: &[u8; 32], password: &str) {
	SEED.lock(|cell| cell.set(*seed));
	set_password(password);
}

pub fn set_password(password: &str) {
	let password = (!password.is_empty()).then(|| password.to_string());
	SERVER_PASSWORD.lock(|cell| *cell.borrow_mut() = password);
}

/// Carries out a Share command from the interface.
pub fn carry_out(command: Command) {
	match command {
		Command::ShareSend { to, user, name } => {
			if SENDS.try_send((to, user, name)).is_err() {
				warn!("Already sending");
			}
		}
		Command::ShareAdmit(admit) => ADMIT.signal(admit),
		Command::SharePassword(password) => PASSWORD.signal(password),
		_ => {}
	}
}

/// What Share is doing, if that changed since last asked.
pub fn status_changed() -> Option<Status> {
	STATUS_CHANGED
		.swap(false, Ordering::Relaxed)
		.then(|| STATUS.lock(|status| status.borrow().clone()))
}

fn update_status(change: impl FnOnce(&mut Status)) {
	STATUS.lock(|status| change(&mut status.borrow_mut()));
	STATUS_CHANGED.store(true, Ordering::Relaxed);
}

/// The network loop's side: routes Share's events, and pulls what came on
/// its streams as the task has room.
#[derive(Default)]
pub struct Pump {
	incoming: VecDeque<Wire>,
	outgoing: VecDeque<Wire>,
}

impl Pump {
	/// Keeps an event of Share's, giving back any other.
	pub fn route(&mut self, event: Event) -> Option<Event> {
		match event {
			Event::StreamAccepted {
				stream: Stream::ShareIncoming,
				from,
			} => self.incoming.push_back(Wire::Accepted(from)),
			Event::StreamClosed(Stream::ShareIncoming) => self.incoming.push_back(Wire::Closed),
			Event::StreamOpened(Stream::Share) => self.outgoing.push_back(Wire::Opened),
			Event::StreamClosed(Stream::Share) => self.outgoing.push_back(Wire::Closed),
			event => return Some(event),
		}
		None
	}

	pub fn pump(&mut self, overlay: &mut Overlay) {
		pump_stream(
			overlay,
			Stream::ShareIncoming,
			&mut self.incoming,
			&INCOMING,
		);
		pump_stream(overlay, Stream::Share, &mut self.outgoing, &OUTGOING);
		INCOMING_BACKLOG.store(
			overlay.stream_backlog(Stream::ShareIncoming),
			Ordering::Relaxed,
		);
		OUTGOING_BACKLOG.store(overlay.stream_backlog(Stream::Share), Ordering::Relaxed);
	}
}

fn pump_stream(overlay: &mut Overlay, stream: Stream, waiting: &mut VecDeque<Wire>, pipe: &Pipe) {
	while let Some(wire) = waiting.pop_front() {
		if let Err(TrySendError::Full(wire)) = pipe.try_send(wire) {
			waiting.push_front(wire);
			return;
		}
	}
	while pipe.free_capacity() > 0 {
		let Some(data) = overlay.stream_receive(stream, PIECE) else {
			return;
		};
		let _ = pipe.try_send(Wire::Data(data));
	}
}

/// One side of a stream as the task sees it: reads take what the network
/// loop handed over, and writes wait while the network has much to send.
struct Connection {
	stream: Stream,
	pipe: &'static Pipe,
	backlog: &'static AtomicUsize,
	data: Vec<u8>,
	read: usize,
	closed: bool,
}

impl Connection {
	fn new(stream: Stream) -> Self {
		let (pipe, backlog) = match stream {
			Stream::ShareIncoming => (&INCOMING, &INCOMING_BACKLOG),
			_ => (&OUTGOING, &OUTGOING_BACKLOG),
		};
		Connection {
			stream,
			pipe,
			backlog,
			data: Vec::new(),
			read: 0,
			closed: false,
		}
	}

	/// Closes the stream, and waits a moment for the network to agree, so
	/// the next session starts afresh.
	async fn close(mut self) {
		FROM_UI.send(Command::CloseStream(self.stream)).await;
		let drained = async {
			while !self.closed {
				if let Wire::Closed = self.pipe.receive().await {
					self.closed = true;
				}
				ROOM.signal(());
			}
		};
		let _ = select(drained, Timer::after(Duration::from_secs(5))).await;
	}
}

impl ErrorType for Connection {
	type Error = ErrorKind;
}

impl Read for Connection {
	async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
		while self.read == self.data.len() {
			if self.closed {
				return Ok(0);
			}
			match self.pipe.receive().await {
				Wire::Data(data) => {
					self.data = data;
					self.read = 0;
				}
				Wire::Closed => self.closed = true,
				Wire::Accepted(_) | Wire::Opened => {}
			}
			ROOM.signal(());
		}
		let count = buf.len().min(self.data.len() - self.read);
		buf[..count].copy_from_slice(&self.data[self.read..self.read + count]);
		self.read += count;
		Ok(count)
	}
}

impl Write for Connection {
	async fn write(&mut self, buf: &[u8]) -> Result<usize, ErrorKind> {
		if self.closed {
			return Err(ErrorKind::BrokenPipe);
		}
		let piece = &buf[..buf.len().min(PIECE)];
		while self.backlog.load(Ordering::Relaxed) + piece.len() > MOST_BACKLOG {
			Timer::after(Duration::from_millis(5)).await;
		}
		self.backlog.fetch_add(piece.len(), Ordering::Relaxed);
		FROM_UI
			.send(Command::StreamSend(self.stream, piece.to_vec()))
			.await;
		Ok(piece.len())
	}

	/// What is written goes to the network at once.
	async fn flush(&mut self) -> Result<(), ErrorKind> {
		Ok(())
	}
}

/// The person at the board, through the interface.
struct Person;

impl Attendant for Person {
	async fn admit(&self, from: Ipv6Addr) -> bool {
		ADMIT.reset();
		update_status(|status| status.asking = Some(from));
		let answer = match select(ADMIT.wait(), Timer::after(ANSWER_TIME)).await {
			Either::First(answer) => answer,
			Either::Second(()) => false,
		};
		update_status(|status| status.asking = None);
		answer
	}

	async fn password(&self, user: &str) -> Option<String> {
		PASSWORD.reset();
		update_status(|status| status.password_for = Some(user.into()));
		let password = match select(PASSWORD.wait(), Timer::after(ANSWER_TIME)).await {
			Either::First(password) => password,
			Either::Second(()) => None,
		};
		update_status(|status| status.password_for = None);
		password
	}

	fn progress(&self, progress: Progress<'_>) {
		update_status(|status| match progress {
			Progress::Receiving { name, bytes } => {
				status.transfer = Some(Transfer {
					name: name.into(),
					bytes,
					total: None,
					incoming: true,
				});
			}
			Progress::Sending { name, bytes, total } => {
				status.transfer = Some(Transfer {
					name: name.into(),
					bytes,
					total: Some(total),
					incoming: false,
				});
			}
			Progress::Received { name, .. } => {
				status.transfer = None;
				status.last = Some(alloc::format!("Received {name}"));
			}
			Progress::Sent { name } => {
				status.transfer = None;
				status.last = Some(alloc::format!("Sent {name}"));
			}
			Progress::Read { .. } => {}
		});
	}
}

/// Serves peers that connect, one at a time. Each session's state is
/// boxed, so takes memory only while it lasts.
#[embassy_executor::task]
pub async fn serve_peers() {
	loop {
		match INCOMING.receive().await {
			Wire::Accepted(from) => Box::pin(serve(from)).await,
			_ => ROOM.signal(()),
		}
	}
}

/// Sends the files asked for, one at a time, boxed as `serve_peers` is.
#[embassy_executor::task]
pub async fn send_files() {
	loop {
		let (to, user, name) = SENDS.receive().await;
		Box::pin(send(to, &user, &name)).await;
	}
}

async fn serve(from: Ipv6Addr) {
	info!("Share connection from {from}");
	let seed = SEED.lock(Cell::get);
	let password = SERVER_PASSWORD.lock(|cell| cell.borrow().clone());
	let mut connection = Connection::new(Stream::ShareIncoming);
	let mut output = Connection::new(Stream::ShareIncoming);
	let mut store = CardStore(Asker::Server);
	let result = share::serve(
		&mut connection,
		&mut output,
		from,
		&seed,
		password.as_deref(),
		&mut store,
		&Person,
	)
	.await;
	if let Err(error) = result {
		info!("Share session with {from} ended: {error:?}");
	}
	update_status(|status| status.transfer = None);
	connection.close().await;
}

async fn send(to: Ipv6Addr, user: &str, name: &str) {
	info!("Sending {name} to {to}");
	update_status(|status| status.last = None);
	let address = SocketAddrV6::new(to, share::PORT, 0, 0);
	FROM_UI
		.send(Command::OpenStream(Stream::Share, address))
		.await;
	let mut connection = Connection::new(Stream::Share);
	let opened = async {
		loop {
			match connection.pipe.receive().await {
				Wire::Opened => return true,
				Wire::Closed => return false,
				_ => ROOM.signal(()),
			}
		}
	};
	let result = match select(opened, Timer::after(CONNECT_TIME)).await {
		Either::First(true) => {
			let mut output = Connection::new(Stream::Share);
			let mut store = CardStore(Asker::Sender);
			share::send(
				&mut connection,
				&mut output,
				user,
				name,
				&mut store,
				&Person,
			)
			.await
		}
		_ => Err(SendError::Failed),
	};
	let failure = match result {
		Ok(()) => None,
		Err(SendError::Refused) => Some("Not let in".into()),
		Err(SendError::Rejected(why)) if !why.is_empty() => Some(why),
		Err(SendError::Rejected(_)) => Some("Refused the file".into()),
		Err(SendError::Store(_)) => Some("Card failed".into()),
		Err(SendError::Failed) => Some("Could not send".into()),
	};
	update_status(|status| {
		status.transfer = None;
		if let Some(failure) = failure {
			status.last = Some(failure);
		}
	});
	connection.close().await;
}

/// A request for the shared folder, which the UI's loop carries out.
pub enum CardRequest {
	List,
	Size(String),
	Read {
		name: String,
		offset: u64,
		length: usize,
	},
	/// Makes the file afresh, answering with the name it got on the card,
	/// under another name if `keep_existing`.
	Create {
		name: String,
		keep_existing: bool,
	},
	Append {
		name: String,
		offset: u64,
		data: Vec<u8>,
	},
}

pub enum CardReply {
	List(Result<Vec<Entry>, StoreError>),
	Size(Result<u64, StoreError>),
	Read(Result<Vec<u8>, StoreError>),
	Create(Result<String, StoreError>),
	Done(Result<(), StoreError>),
}

/// Which task asked the card, to answer.
#[derive(Clone, Copy)]
pub enum Asker {
	Server,
	Sender,
}

pub static CARD_REQUESTS: Channel<CriticalSectionRawMutex, (Asker, CardRequest), 2> =
	Channel::new();
static SERVER_REPLIES: Signal<CriticalSectionRawMutex, CardReply> = Signal::new();
static SENDER_REPLIES: Signal<CriticalSectionRawMutex, CardReply> = Signal::new();

/// Gives the task that asked the card its answer.
pub fn answer_card(asker: Asker, reply: CardReply) {
	match asker {
		Asker::Server => SERVER_REPLIES.signal(reply),
		Asker::Sender => SENDER_REPLIES.signal(reply),
	}
}

async fn ask_card(asker: Asker, request: CardRequest) -> CardReply {
	let replies = match asker {
		Asker::Server => &SERVER_REPLIES,
		Asker::Sender => &SENDER_REPLIES,
	};
	replies.reset();
	CARD_REQUESTS.send((asker, request)).await;
	replies.wait().await
}

/// The shared folder on the card, as one task sees it.
struct CardStore(Asker);

/// The files being read, which a file coming in must not empty.
static READING: CriticalSectionMutex<RefCell<Vec<String>>> =
	CriticalSectionMutex::new(RefCell::new(Vec::new()));

fn is_being_read(name: &str) -> bool {
	READING.lock(|reading| {
		reading
			.borrow()
			.iter()
			.any(|read| read.eq_ignore_ascii_case(name))
	})
}

/// A file in it, gathering writes and reading ahead.
struct CardFile {
	asker: Asker,
	/// Its name on the card.
	name: String,
	/// What was read ahead, and from where.
	ahead: Vec<u8>,
	ahead_from: u64,
	/// What is gathered to write, and where it goes.
	gathered: Vec<u8>,
	gathered_at: u64,
	/// Whether it was opened to read, so is in [`READING`].
	reading: bool,
}

impl CardFile {
	fn new(asker: Asker, name: String) -> Self {
		CardFile {
			asker,
			name,
			ahead: Vec::new(),
			ahead_from: 0,
			gathered: Vec::new(),
			gathered_at: 0,
			reading: false,
		}
	}

	async fn flush(&mut self) -> Result<(), StoreError> {
		if self.gathered.is_empty() {
			return Ok(());
		}
		let data = core::mem::take(&mut self.gathered);
		let offset = self.gathered_at;
		self.gathered_at += data.len() as u64;
		let request = CardRequest::Append {
			name: self.name.clone(),
			offset,
			data,
		};
		match ask_card(self.asker, request).await {
			CardReply::Done(result) => result,
			_ => Err(StoreError::Failed),
		}
	}
}

impl Store for CardStore {
	type File = CardFile;

	async fn list(&mut self) -> Result<Vec<Entry>, StoreError> {
		match ask_card(self.0, CardRequest::List).await {
			CardReply::List(result) => result,
			_ => Err(StoreError::Failed),
		}
	}

	async fn size(&mut self, name: &str) -> Result<u64, StoreError> {
		match ask_card(self.0, CardRequest::Size(name.into())).await {
			CardReply::Size(result) => result,
			_ => Err(StoreError::Failed),
		}
	}

	async fn open(&mut self, name: &str, mode: Mode) -> Result<CardFile, StoreError> {
		match mode {
			Mode::Read => {
				self.size(name).await?;
				READING.lock(|reading| reading.borrow_mut().push(name.into()));
				let mut file = CardFile::new(self.0, name.into());
				file.reading = true;
				Ok(file)
			}
			Mode::Write => {
				let request = CardRequest::Create {
					name: name.into(),
					keep_existing: is_being_read(name),
				};
				match ask_card(self.0, request).await {
					CardReply::Create(result) => Ok(CardFile::new(self.0, result?)),
					_ => Err(StoreError::Failed),
				}
			}
		}
	}

	async fn read(
		&mut self,
		file: &mut CardFile,
		offset: u64,
		buf: &mut [u8],
	) -> Result<usize, StoreError> {
		let ahead_to = file.ahead_from + file.ahead.len() as u64;
		if !(file.ahead_from..ahead_to).contains(&offset) {
			let request = CardRequest::Read {
				name: file.name.clone(),
				offset,
				length: CARD_CHUNK,
			};
			file.ahead = match ask_card(file.asker, request).await {
				CardReply::Read(result) => result?,
				_ => return Err(StoreError::Failed),
			};
			file.ahead_from = offset;
		}
		let start = (offset - file.ahead_from) as usize;
		let count = buf.len().min(file.ahead.len() - start);
		buf[..count].copy_from_slice(&file.ahead[start..start + count]);
		Ok(count)
	}

	async fn write(
		&mut self,
		file: &mut CardFile,
		offset: u64,
		data: &[u8],
	) -> Result<(), StoreError> {
		if offset != file.gathered_at + file.gathered.len() as u64 {
			file.flush().await?;
			file.gathered_at = offset;
		}
		file.gathered.extend_from_slice(data);
		if file.gathered.len() >= CARD_CHUNK {
			file.flush().await?;
		}
		Ok(())
	}

	async fn close(&mut self, mut file: CardFile) -> Result<(), StoreError> {
		if file.reading {
			READING.lock(|reading| {
				let mut reading = reading.borrow_mut();
				if let Some(index) = reading.iter().position(|read| *read == file.name) {
					reading.remove(index);
				}
			});
		}
		file.flush().await
	}
}
