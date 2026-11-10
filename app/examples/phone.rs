//! An IAX2 phone for a computer, using the same code as the boards. Audio
//! is raw 16-bit little-endian mono at 8 kHz on stdin and stdout, so with
//! ALSA's tools it is a softphone:
//!
//! ```sh
//! arecord -q -t raw -f S16_LE -r 8000 -c 1 \
//!   | cargo run -p esp-ygg-app --features voice --example phone -- call 200:1234::1 \
//!   | aplay -q -t raw -f S16_LE -r 8000 -c 1
//! ```
//!
//! `phone call <address>` calls a board, or anything else speaking IAX2.
//! `phone answer` waits on port 4569 and answers the first call. Either way
//! it hangs up when stdin ends, and exits when the call does.
//!
//! `--interface <name>` keeps the call to one network interface, such as a
//! board's hotspot when another interface also routes to Yggdrasil.

use std::io::{self, Read, Write};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant as WallClock};

use esp_ygg_app::voice::iax2::{CallEvent, PORT, Phone};
use esp_ygg_app::voice::{CODECS, Decoder, Encoder, FRAME_SAMPLES};
use yggdrasil_espnow::Instant;

fn main() -> io::Result<()> {
	let mut arguments: Vec<String> = std::env::args().skip(1).collect();
	let interface = match arguments
		.iter()
		.position(|argument| argument == "--interface")
	{
		Some(index) if index + 1 < arguments.len() => {
			let name = arguments.remove(index + 1);
			arguments.remove(index);
			Some(name)
		}
		_ => None,
	};
	let (socket, mut phone) = match arguments.iter().map(String::as_str).collect::<Vec<_>>()[..] {
		["call", address] => {
			let to: Ipv6Addr = address
				.parse()
				.map_err(|_| io::Error::other("not an IPv6 address"))?;
			let socket = bind(0, interface.as_deref())?;
			let mut phone = Phone::new("computer", CODECS, std::process::id() as u16);
			phone
				.call(
					SocketAddrV6::new(to, PORT, 0, 0),
					Instant::from_duration(Duration::ZERO),
				)
				.map_err(|error| io::Error::other(format!("{error:?}")))?;
			(socket, phone)
		}
		["answer"] => (
			bind(PORT, interface.as_deref())?,
			Phone::new("computer", CODECS, std::process::id() as u16),
		),
		_ => {
			eprintln!("usage: phone [--interface <name>] call <address> | answer");
			std::process::exit(2);
		}
	};
	socket.set_read_timeout(Some(Duration::from_millis(5)))?;
	let started = WallClock::now();
	let now = || Instant::from_duration(started.elapsed());
	let microphone = read_frames();
	let mut stdout = io::stdout().lock();
	let mut codecs: Option<(Encoder, Decoder)> = None;
	let mut input_open = true;
	// Frames go out at the pace of real time, even from a file.
	let mut next_frame = WallClock::now();
	let mut buffer = [0u8; 2048];
	loop {
		while let Some((to, datagram)) = phone.poll_transmit() {
			socket.send_to(&datagram, SocketAddr::V6(to))?;
		}
		match socket.recv_from(&mut buffer) {
			Ok((length, SocketAddr::V6(from))) => phone.receive(from, &buffer[..length], now()),
			Ok(_) => {}
			Err(error)
				if matches!(
					error.kind(),
					io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
				) => {}
			Err(error) => return Err(error),
		}
		if phone.poll_at().is_some_and(|due| due <= now()) {
			phone.handle_timeout(now());
		}
		while let Some(event) = phone.poll_event() {
			eprintln!("{event:?}");
			match event {
				CallEvent::Incoming { .. } => {
					let _ = phone.answer(now());
				}
				CallEvent::Ended(_) => {
					// Acknowledge the other side's hangup before leaving.
					while let Some((to, datagram)) = phone.poll_transmit() {
						socket.send_to(&datagram, SocketAddr::V6(to))?;
					}
					return Ok(());
				}
				_ => {}
			}
		}
		if let Some(codec) = phone.codec() {
			let (encoder, _) = codecs.get_or_insert_with(|| {
				eprintln!("Talking in {codec:?}");
				(Encoder::new(codec), Decoder::new(codec))
			});
			// Catch up after a pause rather than sending a burst.
			next_frame = next_frame.max(WallClock::now() - Duration::from_millis(100));
			while WallClock::now() >= next_frame {
				match microphone.try_recv() {
					Ok(frame) => {
						phone.send_voice(&encoder.encode(&frame), now());
						next_frame += Duration::from_millis(20);
					}
					Err(mpsc::TryRecvError::Empty) => break,
					Err(mpsc::TryRecvError::Disconnected) => {
						if input_open {
							input_open = false;
							let _ = phone.hang_up(now());
						}
						break;
					}
				}
			}
		}
		while let Some((codec, data)) = phone.poll_voice() {
			let (_, decoder) =
				codecs.get_or_insert_with(|| (Encoder::new(codec), Decoder::new(codec)));
			let samples = decoder.decode(&data);
			let bytes: Vec<u8> = samples
				.iter()
				.flat_map(|sample| sample.to_le_bytes())
				.collect();
			stdout.write_all(&bytes)?;
			stdout.flush()?;
		}
	}
}

fn bind(port: u16, interface: Option<&str>) -> io::Result<UdpSocket> {
	use socket2::{Domain, Socket, Type};
	let socket = Socket::new(Domain::IPV6, Type::DGRAM, None)?;
	if let Some(interface) = interface {
		socket.bind_device(Some(interface.as_bytes()))?;
	}
	socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
	Ok(socket.into())
}

/// Reads 20 ms frames from stdin on a thread of its own.
fn read_frames() -> mpsc::Receiver<Vec<i16>> {
	let (sender, receiver) = mpsc::sync_channel(50);
	std::thread::spawn(move || {
		let mut stdin = io::stdin().lock();
		let mut bytes = vec![0u8; FRAME_SAMPLES * 2];
		while stdin.read_exact(&mut bytes).is_ok() {
			let frame = bytes
				.as_chunks::<2>()
				.0
				.iter()
				.map(|&pair| i16::from_le_bytes(pair))
				.collect();
			if sender.send(frame).is_err() {
				break;
			}
		}
	});
	receiver
}
