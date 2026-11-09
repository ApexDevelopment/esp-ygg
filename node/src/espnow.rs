//! What goes on air: beacons that announce a node, and frames of the reliable
//! link between two neighbors.

use espnow_reliable::{Link, MAX_DATA_LEN_V2};
use petrified::types::PublicKey;

pub type Mac = [u8; 6];

/// The largest frame [`crate::Node`] sends: the ESP-NOW v2 limit.
pub const FRAME_LEN: usize = MAX_DATA_LEN_V2;

/// Changes whenever the frame format does, so mismatched firmware ignores each other.
pub const BEACON_VERSION: u8 = 1;

const TAG_BEACON: u8 = b'Y';
const TAG_LINK: u8 = b'L';
const BEACON_LEN: usize = 2 + 32;

/// One tag byte comes before every link frame.
pub(crate) type PeerLink = Link<4, { FRAME_LEN - 1 }>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
	Broadcast,
	Unicast(Mac),
}

pub(crate) enum Frame<'a> {
	Beacon(PublicKey),
	Link(&'a [u8]),
}

impl<'a> Frame<'a> {
	pub fn decode(bytes: &'a [u8]) -> Option<Self> {
		match bytes.split_first()? {
			(&TAG_LINK, rest) => Some(Frame::Link(rest)),
			(&TAG_BEACON, rest) if bytes.len() == BEACON_LEN && rest[0] == BEACON_VERSION => {
				Some(Frame::Beacon(PublicKey(rest[1..].try_into().ok()?)))
			}
			_ => None,
		}
	}
}

/// Writes a beacon to `buf` and returns its length.
pub(crate) fn encode_beacon(key: &PublicKey, buf: &mut [u8]) -> usize {
	buf[0] = TAG_BEACON;
	buf[1] = BEACON_VERSION;
	buf[2..BEACON_LEN].copy_from_slice(&key.0);
	BEACON_LEN
}

/// Writes the next frame of `link` to `buf`, after its tag, and returns the
/// total length.
pub(crate) fn encode_link_frame(
	link: &mut PeerLink,
	now: espnow_reliable::Instant,
	buf: &mut [u8],
) -> Option<usize> {
	let len = link.poll_transmit(now, &mut buf[1..])?;
	buf[0] = TAG_LINK;
	Some(len + 1)
}
