//! What the user can change on the device, and how it is stored in flash.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::time::{format_offset, parse_offset};

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
	/// Whether to serve the HTTP status page.
	pub status_page: bool,
	/// The local time zone, in minutes east of UTC.
	pub utc_offset: i16,
	/// Whether to run a WiFi network of the board's own, through which
	/// phones and laptops reach Yggdrasil.
	pub hotspot: bool,
	pub hotspot_name: String,
	/// Empty for an open network. Stored in flash as plain text.
	pub hotspot_password: String,
	/// Which ringtone plays for a call, and how loudly, from 0 for silent.
	pub ringtone: u8,
	pub ring_volume: u8,
}

/// A setting written as text, on screen or in a settings file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSetting {
	Name,
	Channel,
	TimeZone,
	Ssid,
	WifiPassword,
	Peer,
	HotspotName,
	HotspotPassword,
}

impl TextSetting {
	/// The most characters it takes.
	pub fn max_len(self) -> usize {
		match self {
			TextSetting::Name | TextSetting::Ssid | TextSetting::HotspotName => 32,
			TextSetting::Channel => 2,
			TextSetting::TimeZone => 6,
			TextSetting::Peer => 21,
			TextSetting::WifiPassword | TextSetting::HotspotPassword => 63,
		}
	}

	/// Whether it is a password, to be shown as dots.
	pub fn is_secret(self) -> bool {
		matches!(
			self,
			TextSetting::WifiPassword | TextSetting::HotspotPassword
		)
	}

	/// Whether it takes numbers alone, such as an address and port.
	pub fn is_numeric(self) -> bool {
		matches!(
			self,
			TextSetting::Channel | TextSetting::TimeZone | TextSetting::Peer
		)
	}
}

/// The hotspot's name until the user picks another.
pub const DEFAULT_HOTSPOT_NAME: &str = "Yggdrasil";

impl Settings {
	pub fn new() -> Self {
		Settings {
			channel: 1,
			status_page: true,
			hotspot_name: DEFAULT_HOTSPOT_NAME.into(),
			ring_volume: 3,
			..Settings::default()
		}
	}

	/// Whether going from these settings to `new` takes a restart. WiFi is
	/// set up as the board starts, so the bridge and the hotspot are, and
	/// the hotspot holds the channel. The rest apply at once.
	pub fn needs_restart(&self, new: &Settings) -> bool {
		fn bridge(settings: &Settings) -> Option<(&str, &str, &str)> {
			let network = (
				settings.ssid.as_str(),
				settings.password.as_str(),
				settings.peer.as_str(),
			);
			settings.bridge.then_some(network)
		}
		fn hotspot(settings: &Settings) -> Option<(&str, &str, u8)> {
			let network = (
				settings.hotspot_name.as_str(),
				settings.hotspot_password.as_str(),
				settings.channel,
			);
			settings.hotspot.then_some(network)
		}
		bridge(self) != bridge(new) || hotspot(self) != hotspot(new)
	}

	/// Whether bridging has what it needs to start.
	pub fn can_bridge(&self) -> bool {
		!self.ssid.is_empty() && self.peer.parse::<core::net::SocketAddrV4>().is_ok()
	}

	/// Whether a password is one the hotspot can use: none, or what WPA2 takes.
	pub fn is_hotspot_password(password: &str) -> bool {
		password.is_empty() || (8..=63).contains(&password.len())
	}

	/// A setting as text, as it is typed.
	pub fn text(&self, setting: TextSetting) -> String {
		match setting {
			TextSetting::Name => self.name.clone(),
			TextSetting::Channel => self.channel.to_string(),
			TextSetting::TimeZone => format_offset(self.utc_offset),
			TextSetting::Ssid => self.ssid.clone(),
			TextSetting::WifiPassword => self.password.clone(),
			TextSetting::Peer => self.peer.clone(),
			TextSetting::HotspotName => self.hotspot_name.clone(),
			TextSetting::HotspotPassword => self.hotspot_password.clone(),
		}
	}

	/// Sets a setting from text, or says why the text will not do.
	pub fn set_text(&mut self, setting: TextSetting, text: &str) -> Result<(), &'static str> {
		match setting {
			TextSetting::Name => self.name = text.into(),
			TextSetting::Channel => match text.parse() {
				Ok(channel @ 1..=13) => self.channel = channel,
				_ => return Err("Channels go from 1 to 13"),
			},
			TextSetting::TimeZone => {
				self.utc_offset = parse_offset(text).ok_or("Use an offset from UTC, like -05:00")?
			}
			TextSetting::Ssid => self.ssid = text.into(),
			TextSetting::WifiPassword => self.password = text.into(),
			TextSetting::Peer
				if text.is_empty() || text.parse::<core::net::SocketAddrV4>().is_ok() =>
			{
				self.peer = text.into()
			}
			TextSetting::Peer => return Err("Use an IPv4 address and port"),
			TextSetting::HotspotName if text.is_empty() => return Err("The hotspot needs a name"),
			TextSetting::HotspotName => self.hotspot_name = text.into(),
			TextSetting::HotspotPassword if Settings::is_hotspot_password(text) => {
				self.hotspot_password = text.into()
			}
			TextSetting::HotspotPassword => return Err("8 or more characters, or none"),
		}
		Ok(())
	}

	pub fn encode(&self) -> Vec<u8> {
		let mut bytes = Vec::from([VERSION, u8::from(self.bridge), self.channel]);
		for text in [&self.name, &self.ssid, &self.password, &self.peer] {
			push_text(&mut bytes, text);
		}
		bytes.push(u8::from(self.status_page));
		bytes.extend_from_slice(&self.utc_offset.to_le_bytes());
		bytes.push(u8::from(self.hotspot));
		push_text(&mut bytes, &self.hotspot_name);
		push_text(&mut bytes, &self.hotspot_password);
		bytes.extend_from_slice(&[self.ringtone, self.ring_volume]);
		bytes
	}

	/// Returns `None` for anything [`Settings::encode`] did not produce.
	pub fn decode(bytes: &[u8]) -> Option<Self> {
		let (&[VERSION, bridge, channel], mut rest) = bytes.split_first_chunk::<3>()? else {
			return None;
		};
		let (name, ssid, password, peer) = (
			take_text(&mut rest)?,
			take_text(&mut rest)?,
			take_text(&mut rest)?,
			take_text(&mut rest)?,
		);
		// Settings saved before the later fields existed end early.
		let status_page = rest.first().is_none_or(|&byte| byte == 1);
		let utc_offset = rest
			.get(1..3)
			.map_or(0, |bytes| i16::from_le_bytes([bytes[0], bytes[1]]));
		let mut rest = rest.get(3..).unwrap_or_default();
		let hotspot = rest.first() == Some(&1);
		rest = rest.get(1..).unwrap_or_default();
		let hotspot_name = take_text(&mut rest).unwrap_or_else(|| DEFAULT_HOTSPOT_NAME.into());
		let hotspot_password = take_text(&mut rest).unwrap_or_default();
		let defaults = Settings::new();
		let (ringtone, ring_volume) = match rest {
			[ringtone, ring_volume, ..] => (*ringtone, *ring_volume),
			_ => (defaults.ringtone, defaults.ring_volume),
		};
		Some(Settings {
			name,
			ssid,
			password,
			peer,
			bridge: bridge == 1,
			channel,
			status_page,
			utc_offset,
			hotspot,
			hotspot_name,
			hotspot_password,
			ringtone,
			ring_volume,
		})
	}
}

/// Settings as text for a file, one `key = value` per line, which people
/// can read and edit.
impl Settings {
	pub fn to_text(&self) -> String {
		let lines = [
			("name", self.name.clone()),
			("channel", self.channel.to_string()),
			("time_zone", format_offset(self.utc_offset)),
			("status_pages", on_off(self.status_page)),
			("bridge", on_off(self.bridge)),
			("wifi_network", self.ssid.clone()),
			("wifi_password", self.password.clone()),
			("peer", self.peer.clone()),
			("hotspot", on_off(self.hotspot)),
			("hotspot_name", self.hotspot_name.clone()),
			("hotspot_password", self.hotspot_password.clone()),
			("ringtone", self.ringtone.to_string()),
			("ring_volume", self.ring_volume.to_string()),
		];
		lines
			.iter()
			.map(|(key, value)| format!("{key} = {value}\n"))
			.collect()
	}

	/// These settings changed by those in `text`, which may leave some
	/// out. Fails on the first line it cannot use, with why.
	pub fn with_text(&self, text: &str) -> Result<Settings, String> {
		let mut settings = self.clone();
		for (index, line) in text.lines().enumerate() {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				continue;
			}
			let number = index + 1;
			let Some((key, value)) = line.split_once('=') else {
				return Err(format!("Line {number} has no ="));
			};
			settings
				.set(key.trim(), value.trim())
				.map_err(|problem| format!("Line {number}: {problem}"))?;
		}
		Ok(settings)
	}

	fn set(&mut self, key: &str, value: &str) -> Result<(), &'static str> {
		let text_setting = match key {
			"name" => Some(TextSetting::Name),
			"channel" => Some(TextSetting::Channel),
			"time_zone" => Some(TextSetting::TimeZone),
			"wifi_network" => Some(TextSetting::Ssid),
			"wifi_password" => Some(TextSetting::WifiPassword),
			"peer" => Some(TextSetting::Peer),
			"hotspot_name" => Some(TextSetting::HotspotName),
			"hotspot_password" => Some(TextSetting::HotspotPassword),
			_ => None,
		};
		if let Some(setting) = text_setting {
			return self.set_text(setting, value);
		}
		match key {
			"status_pages" => self.status_page = parse_on_off(value)?,
			"bridge" => self.bridge = parse_on_off(value)?,
			"hotspot" => self.hotspot = parse_on_off(value)?,
			"ringtone" => self.ringtone = value.parse().map_err(|_| "bad ringtone")?,
			"ring_volume" => self.ring_volume = value.parse().map_err(|_| "bad volume")?,
			_ => return Err("unknown setting"),
		}
		Ok(())
	}
}

fn on_off(on: bool) -> String {
	String::from(if on { "on" } else { "off" })
}

fn parse_on_off(value: &str) -> Result<bool, &'static str> {
	match value {
		"on" => Ok(true),
		"off" => Ok(false),
		_ => Err("use on or off"),
	}
}

fn push_text(bytes: &mut Vec<u8>, text: &str) {
	let text = &text.as_bytes()[..text.len().min(u8::MAX as usize)];
	bytes.push(text.len() as u8);
	bytes.extend_from_slice(text);
}

/// Takes a length-prefixed string from the front of `rest`.
fn take_text(rest: &mut &[u8]) -> Option<String> {
	let (&length, after) = rest.split_first()?;
	let (field, after) = after.split_at_checked(usize::from(length))?;
	*rest = after;
	String::from_utf8(field.to_vec()).ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn restarts_only_for_wifi() {
		let old = Settings::new();
		let mut new = old.clone();
		new.name = "kitchen".into();
		new.utc_offset = 60;
		new.status_page = !old.status_page;
		new.channel = 11;
		new.ringtone = 2;
		// Unused while the bridge is off
		new.ssid = "PHOTON".into();
		assert!(!old.needs_restart(&new));
		let bridging = Settings {
			bridge: true,
			..new.clone()
		};
		assert!(new.needs_restart(&bridging));
		let other_network = Settings {
			password: "secret".into(),
			..bridging.clone()
		};
		assert!(bridging.needs_restart(&other_network));
		// The hotspot holds the channel.
		let hotspot = Settings {
			hotspot: true,
			..new.clone()
		};
		let other_channel = Settings {
			channel: 1,
			..hotspot.clone()
		};
		assert!(hotspot.needs_restart(&other_channel));
	}

	#[test]
	fn round_trips() {
		let settings = Settings {
			name: "kitchen".into(),
			bridge: true,
			ssid: "PHOTON".into(),
			password: "correct horse".into(),
			peer: "192.168.1.176:9001".into(),
			channel: 6,
			status_page: false,
			utc_offset: -300,
			hotspot: true,
			hotspot_name: "pocket".into(),
			hotspot_password: "hunter22".into(),
			ringtone: 2,
			ring_volume: 5,
		};
		assert_eq!(Settings::decode(&settings.encode()), Some(settings));
	}

	#[test]
	fn reads_settings_saved_before_the_status_page_option() {
		let mut old = Settings::new().encode();
		old.truncate(old.len() - 2 - 3 - 1 - 1 - DEFAULT_HOTSPOT_NAME.len() - 1);
		assert_eq!(Settings::decode(&old), Some(Settings::new()));
	}

	#[test]
	fn rejects_anything_else() {
		assert_eq!(Settings::decode(&[]), None);
		assert_eq!(Settings::decode(&[0xff; 64]), None);
		let mut truncated = Settings::new().encode();
		truncated.truncate(5);
		assert_eq!(Settings::decode(&truncated), None);
	}

	#[test]
	fn text_round_trips() {
		let mut settings = Settings::new();
		settings.name = "kitchen = table".into();
		settings.utc_offset = -300;
		settings.bridge = true;
		settings.peer = "192.168.1.176:9001".into();
		settings.ring_volume = 0;
		assert_eq!(Settings::new().with_text(&settings.to_text()), Ok(settings));
	}

	#[test]
	fn text_changes_only_what_it_names() {
		let text = "# just the network\n\nwifi_network = PHOTON\nwifi_password =  two words \n";
		let settings = Settings::new().with_text(text).unwrap();
		assert_eq!(settings.ssid, "PHOTON");
		assert_eq!(settings.password, "two words");
		assert_eq!(settings.channel, Settings::new().channel);
	}

	#[test]
	fn text_names_the_line_it_cannot_use() {
		let settings = Settings::new();
		assert_eq!(
			settings.with_text("name = a\nchannel = 14"),
			Err("Line 2: Channels go from 1 to 13".into())
		);
		assert_eq!(
			settings.with_text("colour = blue"),
			Err("Line 1: unknown setting".into())
		);
		assert_eq!(settings.with_text("bridge"), Err("Line 1 has no =".into()));
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
