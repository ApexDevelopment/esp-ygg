//! IAX2 (RFC 5456) voice calls, one at a time. This only builds and reads
//! datagrams; the caller moves them over UDP.
//!
//! Yggdrasil already encrypts and authenticates by address, so calls use no
//! IAX2 authentication. The board answers Asterisk's call tokens, and asks
//! for none itself.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{Ipv6Addr, SocketAddrV6};
use core::time::Duration;

use yggdrasil_espnow::Instant;

use super::Codec;

pub const PORT: u16 = 4569;

const VOICE: u8 = 0x02;
const CONTROL: u8 = 0x04;
const IAX: u8 = 0x06;

const NEW: u32 = 0x01;
const PING: u32 = 0x02;
const PONG: u32 = 0x03;
const ACK: u32 = 0x04;
const HANGUP: u32 = 0x05;
const REJECT: u32 = 0x06;
const ACCEPT: u32 = 0x07;
const INVAL: u32 = 0x0a;
const LAGRQ: u32 = 0x0b;
const LAGRP: u32 = 0x0c;
const VNAK: u32 = 0x12;
const TXCNT: u32 = 0x17;
const TXACC: u32 = 0x18;
const POKE: u32 = 0x1e;
const CALLTOKEN: u32 = 0x28;

const CONTROL_HANGUP: u32 = 0x01;
const RINGING: u32 = 0x03;
const ANSWER: u32 = 0x04;
const BUSY: u32 = 0x05;
const CONGESTION: u32 = 0x08;

const IE_CALLED_NUMBER: u8 = 0x01;
const IE_CALLING_NUMBER: u8 = 0x02;
const IE_CALLING_NAME: u8 = 0x04;
const IE_CAPABILITY: u8 = 0x08;
const IE_FORMAT: u8 = 0x09;
const IE_VERSION: u8 = 0x0b;
const IE_CAUSE: u8 = 0x16;
const IE_CALLINGPRES: u8 = 0x26;
const IE_CALLINGTON: u8 = 0x27;
const IE_CALLINGTNS: u8 = 0x28;
const IE_CAUSECODE: u8 = 0x2a;
const IE_CALLTOKEN: u8 = 0x36;

/// Q.931 cause codes
const NORMAL_CLEARING: u8 = 16;
const USER_BUSY: u8 = 17;
const BEARER_NOT_AVAILABLE: u8 = 58;

const FULL_HEADER: usize = 12;
const MINI_HEADER: usize = 4;
/// Waits before each resend of an unacknowledged frame; after the last, the
/// call is given up.
const RESEND_DELAYS: [Duration; 4] = [
	Duration::from_millis(500),
	Duration::from_secs(1),
	Duration::from_secs(2),
	Duration::from_secs(4),
];
const RING_TIMEOUT: Duration = Duration::from_secs(60);
const PING_INTERVAL: Duration = Duration::from_secs(10);
/// One voice frame's length.
const FRAME_MILLIS: u32 = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
	/// Someone is calling. Answer or hang up.
	Incoming {
		from: Ipv6Addr,
		name: Option<String>,
	},
	/// The called side is ringing.
	Ringing,
	Answered,
	/// The call is over, for the reason given.
	Ended(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
	/// A call is already in progress.
	Busy,
	/// No call to act on.
	NoCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
	/// Our NEW is out.
	Calling,
	/// Accepted and ringing, on either side.
	Ringing,
	Up,
	/// Our HANGUP is out; the call is over for the user already.
	Ending,
}

struct Unacked {
	sequence: u8,
	frame: Vec<u8>,
	resends: usize,
	due: Instant,
}

struct Call {
	peer: SocketAddrV6,
	local: u16,
	remote: u16,
	incoming: bool,
	state: State,
	codec: Codec,
	started: Instant,
	out_sequence: u8,
	in_sequence: u8,
	unacked: Vec<Unacked>,
	last_timestamp: u32,
	last_voice_timestamp: Option<u32>,
	ring_deadline: Instant,
	next_ping: Instant,
}

pub struct Phone {
	name: String,
	codecs: Vec<Codec>,
	call: Option<Call>,
	next_call_number: u16,
	transmit: VecDeque<(SocketAddrV6, Vec<u8>)>,
	events: VecDeque<CallEvent>,
	voice: VecDeque<(Codec, Vec<u8>)>,
}

impl Phone {
	/// `codecs` are the ones this side can use, most preferred first.
	pub fn new(name: &str, codecs: &[Codec], seed: u16) -> Self {
		Phone {
			name: name.into(),
			codecs: codecs.to_vec(),
			call: None,
			next_call_number: 1 + seed % 0x7000,
			transmit: VecDeque::new(),
			events: VecDeque::new(),
			voice: VecDeque::new(),
		}
	}

	pub fn call(&mut self, to: SocketAddrV6, now: Instant) -> Result<(), CallError> {
		// A call only waiting for its hangup to be acknowledged is over.
		if self
			.call
			.as_ref()
			.is_some_and(|call| call.state != State::Ending)
		{
			return Err(CallError::Busy);
		}
		let local = self.take_call_number();
		self.call = Some(Call::new(to, local, 0, false, self.codecs[0], now));
		self.send_new(None, now);
		Ok(())
	}

	pub fn answer(&mut self, now: Instant) -> Result<(), CallError> {
		let call = self.call.as_mut().ok_or(CallError::NoCall)?;
		if !(call.incoming && call.state == State::Ringing) {
			return Err(CallError::NoCall);
		}
		call.state = State::Up;
		call.next_ping = now + PING_INTERVAL;
		self.send_reliable(CONTROL, ANSWER, &[], now);
		Ok(())
	}

	/// Hangs up, or declines an incoming call.
	pub fn hang_up(&mut self, now: Instant) -> Result<(), CallError> {
		let call = self.call.as_mut().ok_or(CallError::NoCall)?;
		if call.state == State::Ending {
			return Err(CallError::NoCall);
		}
		call.state = State::Ending;
		let mut data = Vec::new();
		push_ie(&mut data, IE_CAUSE, b"Normal Clearing");
		push_ie(&mut data, IE_CAUSECODE, &[NORMAL_CLEARING]);
		self.send_reliable(IAX, HANGUP, &data, now);
		self.events.push_back(CallEvent::Ended("Hung up".into()));
		Ok(())
	}

	/// The codec voice is sent and expected in, while a call is up.
	pub fn codec(&self) -> Option<Codec> {
		self.call
			.as_ref()
			.filter(|call| call.state == State::Up)
			.map(|call| call.codec)
	}

	/// Sends one 20 ms frame of encoded voice, if the call is up.
	pub fn send_voice(&mut self, data: &[u8], now: Instant) {
		let Some(call) = self.call.as_mut().filter(|call| call.state == State::Up) else {
			return;
		};
		let elapsed = call.elapsed(now);
		let timestamp = match call.last_voice_timestamp {
			Some(last) => (elapsed / FRAME_MILLIS * FRAME_MILLIS).max(last + FRAME_MILLIS),
			None => elapsed / FRAME_MILLIS * FRAME_MILLIS,
		};
		// The first voice frame and each wrap of the short timestamp go in
		// full frames, which name the codec.
		let full = call
			.last_voice_timestamp
			.is_none_or(|last| timestamp as u16 <= last as u16);
		call.last_voice_timestamp = Some(timestamp);
		if full {
			let subclass = call.codec.format();
			let timestamp = timestamp.max(call.last_timestamp + 1);
			self.send_reliable_at(VOICE, subclass, data, timestamp, now);
		} else {
			let mut frame = Vec::with_capacity(MINI_HEADER + data.len());
			frame.extend_from_slice(&call.local.to_be_bytes());
			frame.extend_from_slice(&(timestamp as u16).to_be_bytes());
			frame.extend_from_slice(data);
			self.transmit.push_back((call.peer, frame));
		}
	}

	pub fn receive(&mut self, from: SocketAddrV6, datagram: &[u8], now: Instant) {
		match parse(datagram) {
			Some(Frame::Full(frame)) => self.receive_full(from, &frame, now),
			Some(Frame::Mini { source, data }) => {
				if let Some(call) = &self.call
					&& call.peer == from
					&& call.remote == source
					&& matches!(call.state, State::Up | State::Ringing)
				{
					self.voice.push_back((call.codec, data.to_vec()));
				}
			}
			None => {}
		}
	}

	pub fn poll_transmit(&mut self) -> Option<(SocketAddrV6, Vec<u8>)> {
		self.transmit.pop_front()
	}

	pub fn poll_event(&mut self) -> Option<CallEvent> {
		self.events.pop_front()
	}

	/// Voice that arrived, with its codec.
	pub fn poll_voice(&mut self) -> Option<(Codec, Vec<u8>)> {
		self.voice.pop_front()
	}

	pub fn poll_at(&self) -> Option<Instant> {
		let call = self.call.as_ref()?;
		let resend = call.unacked.iter().map(|frame| frame.due).min();
		let timer = match call.state {
			State::Calling | State::Ringing => Some(call.ring_deadline),
			State::Up => Some(call.next_ping),
			State::Ending => None,
		};
		resend.into_iter().chain(timer).min()
	}

	pub fn handle_timeout(&mut self, now: Instant) {
		let Some(call) = self.call.as_mut() else {
			return;
		};
		let mut lost = false;
		for frame in call.unacked.iter_mut().filter(|frame| frame.due <= now) {
			let Some(delay) = RESEND_DELAYS.get(frame.resends) else {
				lost = true;
				break;
			};
			frame.resends += 1;
			frame.due = now + *delay;
			// Marked as resent, and acknowledging what has come in since.
			frame.frame[2] |= 0x80;
			frame.frame[9] = call.in_sequence;
			self.transmit.push_back((call.peer, frame.frame.clone()));
		}
		if lost {
			self.end("Lost the connection");
			return;
		}
		let (state, incoming) = (call.state, call.incoming);
		let ring_over = now >= call.ring_deadline;
		let ping_due = now >= call.next_ping;
		match state {
			State::Ending if call.unacked.is_empty() => self.call = None,
			State::Calling | State::Ringing if ring_over => {
				let _ = self.hang_up(now);
				self.events.pop_back();
				let reason = if incoming { "Missed call" } else { "No answer" };
				self.events.push_back(CallEvent::Ended(reason.into()));
			}
			State::Up if ping_due => {
				call.next_ping = now + PING_INTERVAL;
				self.send_reliable(IAX, PING, &[], now);
			}
			_ => {}
		}
	}

	fn receive_full(&mut self, from: SocketAddrV6, frame: &Full<'_>, now: Instant) {
		if frame.kind == IAX && frame.subclass == NEW && frame.destination == 0 {
			self.receive_new(from, frame, now);
			return;
		}
		if frame.kind == IAX && frame.subclass == POKE {
			let pong = full_frame(0, frame.source, frame.timestamp, 0, 1, IAX, PONG, &[]);
			self.transmit.push_back((from, pong));
			return;
		}
		let Some(call) = self.call.as_mut().filter(|call| {
			call.local == frame.destination
				&& (call.peer == from || (call.remote == 0 && same_node(&call.peer, &from)))
		}) else {
			return;
		};
		if call.remote == 0 {
			// The first reply names the other side's call, and the address
			// it answers from, which may be another of the same node's.
			call.remote = frame.source;
			call.peer = from;
		}
		call.acknowledged(frame.in_sequence);
		if frame.kind == IAX && frame.subclass == CALLTOKEN {
			let token = ies(frame.data)
				.find(|(id, _)| *id == IE_CALLTOKEN)
				.map(|(_, token)| token.to_vec());
			if let Some(token) = token.filter(|_| call.state == State::Calling) {
				call.remote = 0;
				call.out_sequence = 0;
				call.in_sequence = 0;
				call.unacked.clear();
				self.send_new(Some(&token), now);
			}
			return;
		}
		if frame.kind == IAX && matches!(frame.subclass, ACK | INVAL | VNAK | TXCNT | TXACC) {
			match frame.subclass {
				INVAL => self.end("The other side dropped the call"),
				VNAK => {
					let call = self.call.as_mut().expect("checked above");
					for unacked in &mut call.unacked {
						unacked.due = now;
					}
				}
				_ => {}
			}
			if self
				.call
				.as_ref()
				.is_some_and(|call| call.state == State::Ending && call.unacked.is_empty())
			{
				self.call = None;
			}
			return;
		}
		if frame.out_sequence != call.in_sequence {
			// A resend of something already handled needs acknowledging
			// again; anything ahead waits for its predecessors.
			let behind = call.in_sequence.wrapping_sub(frame.out_sequence) <= 128;
			if behind {
				self.send_ack(frame.timestamp);
			}
			return;
		}
		call.in_sequence = call.in_sequence.wrapping_add(1);
		self.dispatch(frame, now);
	}

	fn dispatch(&mut self, frame: &Full<'_>, now: Instant) {
		let call = self.call.as_mut().expect("checked by the caller");
		match (frame.kind, frame.subclass) {
			(IAX, PING) => self.send_reliable_at(IAX, PONG, &[], frame.timestamp, now),
			(IAX, LAGRQ) => self.send_reliable_at(IAX, LAGRP, &[], frame.timestamp, now),
			(IAX, ACCEPT) if call.state == State::Calling => {
				let format = ies(frame.data)
					.find(|(id, _)| *id == IE_FORMAT)
					.and_then(|(_, value)| value.try_into().ok())
					.map(u32::from_be_bytes);
				match format.and_then(Codec::from_format) {
					Some(codec) if self.codecs.contains(&codec) => {
						call.codec = codec;
						call.state = State::Ringing;
						self.send_ack(frame.timestamp);
					}
					_ => {
						self.send_ack(frame.timestamp);
						let _ = self.hang_up(now);
						self.events.pop_back();
						self.events
							.push_back(CallEvent::Ended("No codec in common".into()));
					}
				}
			}
			(IAX, HANGUP) => {
				self.send_ack(frame.timestamp);
				self.end("The other side hung up");
			}
			(IAX, REJECT) => {
				let cause = ies(frame.data)
					.find(|(id, _)| *id == IE_CAUSE)
					.and_then(|(_, cause)| core::str::from_utf8(cause).ok())
					.filter(|cause| !cause.is_empty())
					.map_or_else(|| "Call refused".to_string(), String::from);
				self.send_ack(frame.timestamp);
				self.end(&cause);
			}
			(CONTROL, RINGING) => {
				let incoming = call.incoming;
				self.send_ack(frame.timestamp);
				if !incoming {
					self.events.push_back(CallEvent::Ringing);
				}
			}
			(CONTROL, ANSWER) if !call.incoming && call.state != State::Ending => {
				call.state = State::Up;
				call.next_ping = now + PING_INTERVAL;
				self.send_ack(frame.timestamp);
				self.events.push_back(CallEvent::Answered);
			}
			(CONTROL, CONTROL_HANGUP) => {
				self.send_ack(frame.timestamp);
				self.end("The other side hung up");
			}
			(CONTROL, BUSY) => {
				self.send_ack(frame.timestamp);
				self.end("Busy");
			}
			(CONTROL, CONGESTION) => {
				self.send_ack(frame.timestamp);
				self.end("The other side is congested");
			}
			(VOICE, format) => {
				if let Some(codec) = Codec::from_format(format) {
					call.codec = codec;
					self.voice.push_back((codec, frame.data.to_vec()));
				}
				self.send_ack(frame.timestamp);
			}
			_ => self.send_ack(frame.timestamp),
		}
	}

	fn receive_new(&mut self, from: SocketAddrV6, frame: &Full<'_>, now: Instant) {
		if let Some(call) = &self.call {
			// A resent NEW for the call already set up
			if call.peer == from && call.remote == frame.source && call.incoming {
				self.send_ack(frame.timestamp);
				return;
			}
			self.refuse(from, frame, "Busy", USER_BUSY);
			return;
		}
		let mut capability = 0;
		let mut preferred = 0;
		let mut name = None;
		for (id, value) in ies(frame.data) {
			match id {
				IE_CAPABILITY => capability = be_u32(value),
				IE_FORMAT => preferred = be_u32(value),
				IE_CALLING_NAME => name = core::str::from_utf8(value).ok().map(String::from),
				_ => {}
			}
		}
		let offered = capability | preferred;
		let codec = Codec::from_format(preferred)
			.filter(|codec| self.codecs.contains(codec))
			.or_else(|| {
				self.codecs
					.iter()
					.copied()
					.find(|codec| offered & codec.format() != 0)
			});
		let Some(codec) = codec else {
			self.refuse(from, frame, "No codec in common", BEARER_NOT_AVAILABLE);
			return;
		};
		let local = self.take_call_number();
		let mut call = Call::new(from, local, frame.source, true, codec, now);
		call.in_sequence = 1;
		call.state = State::Ringing;
		self.call = Some(call);
		let mut accept = Vec::new();
		push_ie(&mut accept, IE_FORMAT, &codec.format().to_be_bytes());
		self.send_reliable(IAX, ACCEPT, &accept, now);
		self.send_reliable(CONTROL, RINGING, &[], now);
		self.events.push_back(CallEvent::Incoming {
			from: *from.ip(),
			name: name.filter(|name| !name.is_empty()),
		});
	}

	/// Turns down a NEW without keeping any state for it.
	fn refuse(&mut self, to: SocketAddrV6, frame: &Full<'_>, cause: &str, code: u8) {
		let mut data = Vec::new();
		push_ie(&mut data, IE_CAUSE, cause.as_bytes());
		push_ie(&mut data, IE_CAUSECODE, &[code]);
		let local = self.take_call_number();
		let reject = full_frame(
			local,
			frame.source,
			frame.timestamp,
			0,
			1,
			IAX,
			REJECT,
			&data,
		);
		self.transmit.push_back((to, reject));
	}

	fn send_new(&mut self, token: Option<&[u8]>, now: Instant) {
		let mut data = Vec::new();
		push_ie(&mut data, IE_VERSION, &2u16.to_be_bytes());
		push_ie(&mut data, IE_CALLED_NUMBER, b"s");
		push_ie(&mut data, IE_CALLING_NAME, self.name.as_bytes());
		push_ie(&mut data, IE_CALLING_NUMBER, b"");
		push_ie(&mut data, IE_CALLINGPRES, &[0]);
		push_ie(&mut data, IE_CALLINGTON, &[0]);
		push_ie(&mut data, IE_CALLINGTNS, &[0, 0]);
		let capability = self
			.codecs
			.iter()
			.fold(0, |all, codec| all | codec.format());
		push_ie(&mut data, IE_FORMAT, &self.codecs[0].format().to_be_bytes());
		push_ie(&mut data, IE_CAPABILITY, &capability.to_be_bytes());
		push_ie(&mut data, IE_CALLTOKEN, token.unwrap_or_default());
		self.send_reliable(IAX, NEW, &data, now);
	}

	fn send_reliable(&mut self, kind: u8, subclass: u32, data: &[u8], now: Instant) {
		let Some(call) = self.call.as_mut() else {
			return;
		};
		let timestamp = call.elapsed(now).max(call.last_timestamp + 1);
		self.send_reliable_at(kind, subclass, data, timestamp, now);
	}

	fn send_reliable_at(
		&mut self,
		kind: u8,
		subclass: u32,
		data: &[u8],
		timestamp: u32,
		now: Instant,
	) {
		let Some(call) = self.call.as_mut() else {
			return;
		};
		call.last_timestamp = call.last_timestamp.max(timestamp);
		let frame = full_frame(
			call.local,
			call.remote,
			timestamp,
			call.out_sequence,
			call.in_sequence,
			kind,
			subclass,
			data,
		);
		call.unacked.push(Unacked {
			sequence: call.out_sequence,
			frame: frame.clone(),
			resends: 0,
			due: now + RESEND_DELAYS[0],
		});
		call.out_sequence = call.out_sequence.wrapping_add(1);
		self.transmit.push_back((call.peer, frame));
	}

	fn send_ack(&mut self, timestamp: u32) {
		let Some(call) = self.call.as_ref() else {
			return;
		};
		let ack = full_frame(
			call.local,
			call.remote,
			timestamp,
			call.out_sequence,
			call.in_sequence,
			IAX,
			ACK,
			&[],
		);
		self.transmit.push_back((call.peer, ack));
	}

	/// Ends the call at once, without telling the other side.
	fn end(&mut self, reason: &str) {
		if let Some(call) = self.call.take()
			&& call.state != State::Ending
		{
			self.events.push_back(CallEvent::Ended(reason.into()));
		}
	}

	fn take_call_number(&mut self) -> u16 {
		let number = self.next_call_number;
		self.next_call_number = if number >= 0x7fff { 1 } else { number + 1 };
		number
	}
}

impl Call {
	fn new(
		peer: SocketAddrV6,
		local: u16,
		remote: u16,
		incoming: bool,
		codec: Codec,
		now: Instant,
	) -> Self {
		Call {
			peer,
			local,
			remote,
			incoming,
			state: State::Calling,
			codec,
			started: now,
			out_sequence: 0,
			in_sequence: 0,
			unacked: Vec::new(),
			last_timestamp: 0,
			last_voice_timestamp: None,
			ring_deadline: now + RING_TIMEOUT,
			next_ping: now + PING_INTERVAL,
		}
	}

	fn elapsed(&self, now: Instant) -> u32 {
		now.saturating_duration_since(self.started).as_millis() as u32
	}

	/// Drops the frames the other side has had, up to `next`, the next
	/// sequence number it expects.
	fn acknowledged(&mut self, next: u8) {
		self.unacked.retain(|frame| {
			let ahead = next.wrapping_sub(frame.sequence);
			!(1..=128).contains(&ahead)
		});
	}
}

struct Full<'a> {
	source: u16,
	destination: u16,
	timestamp: u32,
	out_sequence: u8,
	in_sequence: u8,
	kind: u8,
	subclass: u32,
	data: &'a [u8],
}

enum Frame<'a> {
	Full(Full<'a>),
	Mini { source: u16, data: &'a [u8] },
}

fn parse(datagram: &[u8]) -> Option<Frame<'_>> {
	let first = u16::from_be_bytes(datagram.get(..2)?.try_into().ok()?);
	if first & 0x8000 == 0 {
		// Meta frames start with a zero call number.
		if first == 0 || datagram.len() < MINI_HEADER {
			return None;
		}
		return Some(Frame::Mini {
			source: first,
			data: &datagram[MINI_HEADER..],
		});
	}
	if datagram.len() < FULL_HEADER {
		return None;
	}
	let subclass = datagram[11];
	let subclass = if subclass & 0x80 != 0 {
		1u32.checked_shl(u32::from(subclass & 0x7f))?
	} else {
		u32::from(subclass)
	};
	Some(Frame::Full(Full {
		source: first & 0x7fff,
		destination: u16::from_be_bytes([datagram[2], datagram[3]]) & 0x7fff,
		timestamp: u32::from_be_bytes(datagram[4..8].try_into().ok()?),
		out_sequence: datagram[8],
		in_sequence: datagram[9],
		kind: datagram[10],
		subclass,
		data: &datagram[FULL_HEADER..],
	}))
}

#[allow(clippy::too_many_arguments)]
fn full_frame(
	source: u16,
	destination: u16,
	timestamp: u32,
	out_sequence: u8,
	in_sequence: u8,
	kind: u8,
	subclass: u32,
	data: &[u8],
) -> Vec<u8> {
	let mut frame = Vec::with_capacity(FULL_HEADER + data.len());
	frame.extend_from_slice(&(0x8000 | source).to_be_bytes());
	frame.extend_from_slice(&destination.to_be_bytes());
	frame.extend_from_slice(&timestamp.to_be_bytes());
	frame.extend_from_slice(&[out_sequence, in_sequence, kind, encode_subclass(subclass)]);
	frame.extend_from_slice(data);
	frame
}

/// Whether two addresses are surely the same node: they are equal, or in
/// the same Yggdrasil subnet, which only that node's key can use. A computer
/// called on one address in its subnet may answer from another, such as an
/// IPv6 privacy address.
fn same_node(a: &SocketAddrV6, b: &SocketAddrV6) -> bool {
	let (a, b) = (a.ip().octets(), b.ip().octets());
	a == b || (a[0] == 0x03 && a[..8] == b[..8])
}

/// A subclass in 7 bits, or as a power of two with the C bit for larger
/// formats.
fn encode_subclass(subclass: u32) -> u8 {
	if subclass < 0x80 {
		subclass as u8
	} else {
		0x80 | subclass.trailing_zeros() as u8
	}
}

fn push_ie(data: &mut Vec<u8>, id: u8, value: &[u8]) {
	let value = &value[..value.len().min(255)];
	data.extend_from_slice(&[id, value.len() as u8]);
	data.extend_from_slice(value);
}

/// The information elements in a frame's data.
fn ies(mut data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
	core::iter::from_fn(move || {
		let (&[id, length], rest) = data.split_first_chunk::<2>()?;
		let (value, rest) = rest.split_at_checked(usize::from(length))?;
		data = rest;
		Some((id, value))
	})
}

fn be_u32(value: &[u8]) -> u32 {
	value.try_into().map_or(0, u32::from_be_bytes)
}

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	fn at(millis: u64) -> Instant {
		Instant::from_duration(Duration::from_millis(millis))
	}

	fn address(last: u16) -> SocketAddrV6 {
		SocketAddrV6::new(Ipv6Addr::new(0x200, 0, 0, 0, 0, 0, 0, last), PORT, 0, 0)
	}

	/// Two phones, passing everything between them unless `drop` says not to.
	struct Line {
		alice: Phone,
		bob: Phone,
		now: u64,
	}

	impl Line {
		fn new() -> Self {
			Line {
				alice: Phone::new("alice", &[Codec::Ulaw], 1),
				bob: Phone::new("bob", &[Codec::Ulaw], 2),
				now: 0,
			}
		}

		fn run(&mut self, millis: u64, mut drop: impl FnMut(&[u8]) -> bool) {
			for _ in 0..millis {
				self.now += 1;
				let now = at(self.now);
				for phone in [&mut self.alice, &mut self.bob] {
					if phone.poll_at().is_some_and(|due| due <= now) {
						phone.handle_timeout(now);
					}
				}
				while let Some((to, datagram)) = self.alice.poll_transmit() {
					assert_eq!(to, address(2));
					if !drop(&datagram) {
						self.bob.receive(address(1), &datagram, now);
					}
				}
				while let Some((to, datagram)) = self.bob.poll_transmit() {
					assert_eq!(to, address(1));
					if !drop(&datagram) {
						self.alice.receive(address(2), &datagram, now);
					}
				}
			}
		}

		fn events(phone: &mut Phone) -> Vec<CallEvent> {
			core::iter::from_fn(|| phone.poll_event()).collect()
		}
	}

	#[test]
	fn calls_answers_talks_and_hangs_up() {
		let mut line = Line::new();
		line.alice.call(address(2), at(0)).unwrap();
		line.run(10, |_| false);
		assert_eq!(
			Line::events(&mut line.bob),
			[CallEvent::Incoming {
				from: *address(1).ip(),
				name: Some("alice".into())
			}]
		);
		assert_eq!(Line::events(&mut line.alice), [CallEvent::Ringing]);

		line.bob.answer(at(line.now)).unwrap();
		line.run(10, |_| false);
		assert_eq!(Line::events(&mut line.alice), [CallEvent::Answered]);
		assert_eq!(line.alice.codec(), Some(Codec::Ulaw));

		for frame in 0..3u8 {
			line.alice.send_voice(&[frame; 160], at(line.now));
			line.run(20, |_| false);
		}
		let heard: Vec<_> = core::iter::from_fn(|| line.bob.poll_voice()).collect();
		assert_eq!(
			heard,
			[0u8, 1, 2].map(|frame| (Codec::Ulaw, vec![frame; 160]))
		);

		line.bob.hang_up(at(line.now)).unwrap();
		line.run(10, |_| false);
		assert_eq!(
			Line::events(&mut line.bob),
			[CallEvent::Ended("Hung up".into())]
		);
		assert_eq!(
			Line::events(&mut line.alice),
			[CallEvent::Ended("The other side hung up".into())]
		);
		assert!(line.alice.call.is_none() && line.bob.call.is_none());
	}

	#[test]
	fn resends_what_is_lost() {
		let mut line = Line::new();
		line.alice.call(address(2), at(0)).unwrap();
		let mut first = true;
		// Lose the first NEW.
		line.run(2000, |datagram| {
			core::mem::take(&mut first) && u32::from(datagram[11]) == NEW
		});
		assert!(matches!(
			Line::events(&mut line.bob)[..],
			[CallEvent::Incoming { .. }]
		));
		line.bob.answer(at(line.now)).unwrap();
		line.run(10, |_| false);
		assert_eq!(
			Line::events(&mut line.alice),
			[CallEvent::Ringing, CallEvent::Answered]
		);
	}

	#[test]
	fn gives_up_on_a_silent_peer() {
		let mut line = Line::new();
		line.alice.call(address(2), at(0)).unwrap();
		line.run(10_000, |_| true);
		assert_eq!(
			Line::events(&mut line.alice),
			[CallEvent::Ended("Lost the connection".into())]
		);
	}

	#[test]
	fn takes_an_answer_from_another_address_of_the_same_subnet() {
		let subnet =
			|last| SocketAddrV6::new(Ipv6Addr::new(0x300, 1, 2, 3, 0, 0, 0, last), PORT, 0, 0);
		let mut alice = Phone::new("alice", &[Codec::Ulaw], 1);
		let mut bob = Phone::new("bob", &[Codec::Ulaw], 2);
		alice.call(subnet(1), at(0)).unwrap();
		let (_, new) = alice.poll_transmit().unwrap();
		bob.receive(address(1), &new, at(1));
		while let Some((_, datagram)) = bob.poll_transmit() {
			// Bob answers from his privacy address.
			alice.receive(subnet(2), &datagram, at(2));
		}
		assert_eq!(Line::events(&mut alice), [CallEvent::Ringing]);
		// But a stranger cannot answer for him.
		let mut alice = Phone::new("alice", &[Codec::Ulaw], 1);
		alice.call(subnet(1), at(0)).unwrap();
		let (_, new) = alice.poll_transmit().unwrap();
		let mut bob = Phone::new("bob", &[Codec::Ulaw], 2);
		bob.receive(address(1), &new, at(1));
		while let Some((_, datagram)) = bob.poll_transmit() {
			alice.receive(address(9), &datagram, at(2));
		}
		assert_eq!(Line::events(&mut alice), []);
	}

	#[test]
	fn turns_away_a_second_caller() {
		let mut line = Line::new();
		line.alice.call(address(2), at(0)).unwrap();
		line.run(10, |_| false);
		let mut carol = Phone::new("carol", &[Codec::Ulaw], 3);
		carol.call(address(2), at(line.now)).unwrap();
		let (_, new) = carol.poll_transmit().unwrap();
		line.bob.receive(address(3), &new, at(line.now));
		let reject = core::iter::from_fn(|| line.bob.poll_transmit())
			.find(|(to, _)| *to == address(3))
			.map(|(_, datagram)| datagram)
			.expect("a reply to carol");
		carol.receive(address(2), &reject, at(line.now));
		assert_eq!(Line::events(&mut carol), [CallEvent::Ended("Busy".into())]);
	}

	#[test]
	fn answers_a_call_token_challenge() {
		let mut alice = Phone::new("alice", &[Codec::Ulaw], 1);
		alice.call(address(2), at(0)).unwrap();
		let (_, new) = alice.poll_transmit().unwrap();
		let Some(Frame::Full(new)) = parse(&new) else {
			panic!("NEW is a full frame");
		};
		assert_eq!(ies(new.data).next(), Some((IE_VERSION, &[0, 2][..])));
		let mut token = Vec::new();
		push_ie(&mut token, IE_CALLTOKEN, b"opaque");
		let challenge = full_frame(0, new.source, 1, 0, 1, IAX, CALLTOKEN, &token);
		alice.receive(address(2), &challenge, at(1));
		let (_, again) = alice.poll_transmit().unwrap();
		let Some(Frame::Full(again)) = parse(&again) else {
			panic!("NEW is a full frame");
		};
		assert_eq!((again.subclass, again.out_sequence), (NEW, 0));
		assert!(ies(again.data).any(|ie| ie == (IE_CALLTOKEN, &b"opaque"[..])));
	}

	#[test]
	fn reads_frames_as_the_rfc_lays_them_out() {
		// A full ACK from call 2 to call 1, and a mini frame from call 5.
		let ack = [0x80, 0x02, 0x00, 0x01, 0, 0, 0x01, 0x00, 3, 4, 0x06, 0x04];
		let Some(Frame::Full(frame)) = parse(&ack) else {
			panic!("a full frame");
		};
		assert_eq!(
			(frame.source, frame.destination, frame.timestamp),
			(2, 1, 256)
		);
		assert_eq!((frame.out_sequence, frame.in_sequence), (3, 4));
		assert_eq!((frame.kind, frame.subclass), (IAX, ACK));
		let Some(Frame::Mini { source, data }) = parse(&[0x00, 0x05, 0x12, 0x34, 9]) else {
			panic!("a mini frame");
		};
		assert_eq!((source, data), (5, &[9][..]));
		// A subclass with the C bit is a power of two.
		let mut voice = ack;
		voice[10] = VOICE;
		voice[11] = 0x84;
		let Some(Frame::Full(frame)) = parse(&voice) else {
			panic!("a full frame");
		};
		assert_eq!(frame.subclass, 0x10);
		// And larger formats are written that way.
		assert_eq!(encode_subclass(0x800), 0x8b);
		assert_eq!(encode_subclass(0x04), 0x04);
	}
}
