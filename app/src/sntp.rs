//! Simple Network Time Protocol messages (RFC 4330), enough to ask for the
//! time and to answer others who ask.

pub const PORT: u16 = 123;
pub const MESSAGE_LEN: usize = 48;

/// Seconds from 1900, where NTP counts from, to 1970.
const UNIX_EPOCH_IN_NTP: u64 = 2_208_988_800;
const VERSION: u8 = 4;
const MODE_CLIENT: u8 = 3;
const MODE_SERVER: u8 = 4;
/// Leap indicator 3 means the server's clock is not synchronized.
const UNSYNCHRONIZED: u8 = 3;

/// A time reported by a server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
	/// Microseconds since the Unix epoch.
	pub unix_micros: u64,
	/// How many steps the server is from a reference clock.
	pub stratum: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
	Request,
	Reply(Reply),
}

pub fn request() -> [u8; MESSAGE_LEN] {
	let mut message = [0u8; MESSAGE_LEN];
	message[0] = VERSION << 3 | MODE_CLIENT;
	message
}

/// The answer to a request, telling the client the time is `unix_micros`
/// from a clock `stratum` steps from a reference.
pub fn reply(request: &[u8], unix_micros: u64, stratum: u8) -> [u8; MESSAGE_LEN] {
	let mut message = [0u8; MESSAGE_LEN];
	let version = request.first().map_or(VERSION, |first| first >> 3 & 0x07);
	message[0] = version << 3 | MODE_SERVER;
	message[1] = stratum;
	let timestamp = to_ntp(unix_micros);
	// Originate: the client's transmit time, so it can match the reply.
	if let Some(transmit) = request.get(40..48) {
		message[24..32].copy_from_slice(transmit);
	}
	message[32..40].copy_from_slice(&timestamp);
	message[40..48].copy_from_slice(&timestamp);
	message
}

pub fn parse(message: &[u8]) -> Option<Message> {
	if message.len() < MESSAGE_LEN {
		return None;
	}
	match message[0] & 0x07 {
		MODE_CLIENT => Some(Message::Request),
		MODE_SERVER => {
			let stratum = message[1];
			if message[0] >> 6 == UNSYNCHRONIZED || stratum == 0 || stratum >= 16 {
				return None;
			}
			let unix_micros = from_ntp(message[40..48].try_into().ok()?)?;
			Some(Message::Reply(Reply {
				unix_micros,
				stratum,
			}))
		}
		_ => None,
	}
}

fn to_ntp(unix_micros: u64) -> [u8; 8] {
	let seconds = unix_micros / 1_000_000 + UNIX_EPOCH_IN_NTP;
	let fraction = (unix_micros % 1_000_000) * (1 << 32) / 1_000_000;
	let mut bytes = [0u8; 8];
	bytes[..4].copy_from_slice(&(seconds as u32).to_be_bytes());
	bytes[4..].copy_from_slice(&(fraction as u32).to_be_bytes());
	bytes
}

fn from_ntp(bytes: [u8; 8]) -> Option<u64> {
	let seconds = u64::from(u32::from_be_bytes(bytes[..4].try_into().ok()?));
	let fraction = u64::from(u32::from_be_bytes(bytes[4..].try_into().ok()?));
	// Era 0 ends in 2036; later times wrap to small values.
	let seconds = if seconds < UNIX_EPOCH_IN_NTP {
		seconds + (1 << 32)
	} else {
		seconds
	};
	Some((seconds - UNIX_EPOCH_IN_NTP) * 1_000_000 + fraction * 1_000_000 / (1 << 32))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn answers_requests_with_the_time() {
		let request = request();
		assert_eq!(parse(&request), Some(Message::Request));
		let time = 1_794_000_000_123_456;
		let reply = reply(&request, time, 3);
		let Some(Message::Reply(parsed)) = parse(&reply) else {
			panic!("not a reply");
		};
		assert_eq!(parsed.stratum, 3);
		assert!(parsed.unix_micros.abs_diff(time) <= 1);
	}

	#[test]
	fn reads_times_after_2036() {
		let time = 2_200_000_000_000_000;
		let bytes = to_ntp(time);
		assert!(from_ntp(bytes).unwrap().abs_diff(time) <= 1);
	}

	#[test]
	fn ignores_unsynchronized_servers() {
		let mut reply = reply(&request(), 1_794_000_000_000_000, 2);
		reply[0] |= UNSYNCHRONIZED << 6;
		assert_eq!(parse(&reply), None);
		assert_eq!(parse(&[0u8; 10]), None);
	}
}
