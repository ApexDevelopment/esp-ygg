//! What the user can change on the device, and how it is stored in flash.

use alloc::string::String;
use alloc::vec::Vec;

const VERSION: u8 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
	/// Shown in nodeinfo, on the status page and on screen.
	pub name: String,
	/// Whether to join WiFi and peer with another node over TCP, connecting
	/// the ESP-NOW mesh to the wider network.
	pub bridge: bool,
	pub ssid: String,
	/// Stored in flash as plain text.
	pub password: String,
	/// The node to peer with over TCP, as an IPv4 address and port.
	pub peer: String,
	/// The WiFi channel for ESP-NOW when not bridging. When bridging, the
	/// access point decides.
	pub channel: u8,
}

impl Settings {
	pub fn new() -> Self {
		Settings {
			channel: 1,
			..Settings::default()
		}
	}

	/// Whether bridging has what it needs to start.
	pub fn can_bridge(&self) -> bool {
		!self.ssid.is_empty() && self.peer.parse::<core::net::SocketAddrV4>().is_ok()
	}

	pub fn encode(&self) -> Vec<u8> {
		let mut bytes = Vec::from([VERSION, u8::from(self.bridge), self.channel]);
		for text in [&self.name, &self.ssid, &self.password, &self.peer] {
			let text = &text.as_bytes()[..text.len().min(u8::MAX as usize)];
			bytes.push(text.len() as u8);
			bytes.extend_from_slice(text);
		}
		bytes
	}

	/// Returns `None` for anything [`Settings::encode`] did not produce.
	pub fn decode(bytes: &[u8]) -> Option<Self> {
		let (&[VERSION, bridge, channel], mut rest) = bytes.split_first_chunk::<3>()? else {
			return None;
		};
		let mut text = || {
			let (&length, after) = rest.split_first()?;
			let (field, after) = after.split_at_checked(usize::from(length))?;
			rest = after;
			String::from_utf8(field.to_vec()).ok()
		};
		Some(Settings {
			name: text()?,
			ssid: text()?,
			password: text()?,
			peer: text()?,
			bridge: bridge == 1,
			channel,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trips() {
		let settings = Settings {
			name: "kitchen".into(),
			bridge: true,
			ssid: "PHOTON".into(),
			password: "correct horse".into(),
			peer: "192.168.1.176:9001".into(),
			channel: 6,
		};
		assert_eq!(Settings::decode(&settings.encode()), Some(settings));
	}

	#[test]
	fn rejects_anything_else() {
		assert_eq!(Settings::decode(&[]), None);
		assert_eq!(Settings::decode(&[0xff; 64]), None);
		let mut truncated = Settings::new().encode();
		truncated.pop();
		assert_eq!(Settings::decode(&truncated), None);
	}

	#[test]
	fn bridging_needs_a_network_and_peer() {
		let mut settings = Settings::new();
		assert!(!settings.can_bridge());
		settings.ssid = "home".into();
		settings.peer = "192.168.1.10:9001".into();
		assert!(settings.can_bridge());
		settings.peer = "desktop:9001".into();
		assert!(!settings.can_bridge());
	}
}
