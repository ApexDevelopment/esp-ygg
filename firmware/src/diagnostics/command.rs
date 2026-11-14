//! Parses the serial diagnostic commands without touching hardware.

use core::net::SocketAddrV4;

#[derive(Debug, PartialEq)]
pub enum Command<'a> {
	Help,
	Status,
	Watch(bool),
	Radio(bool),
	Pause(u16),
	Priority(usize),
	Reboot,
	Wifi(WifiCommand),
	Dns(&'a str),
	Ntp,
	Tcp(Option<SocketAddrV4>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WifiCommand {
	Status,
	Enabled(bool),
	Reconnect,
	Reset,
	Scan {
		active: bool,
		channel: Option<u8>,
		dwell: u16,
	},
	Power(i8),
	PowerSave(PowerSave),
	Protocols(Protocols),
	Bandwidth(u16),
	Bssid(Option<[u8; 6]>),
	Channel(Option<u8>),
	AllChannels(bool),
	MixedAuthentication(bool),
	Sniff(bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PowerSave {
	None,
	Min,
	Max,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocols {
	B,
	Bg,
	Bgn,
	Bgnax,
}

pub fn parse(text: &str) -> Result<Command<'_>, &'static str> {
	let mut words = text.split_ascii_whitespace();
	if words.next() != Some("diag") {
		return Err("expected diag");
	}
	let command = match words.next() {
		None | Some("status") => Command::Status,
		Some("help") => Command::Help,
		Some("watch") => Command::Watch(on_off(words.next())?),
		Some("radio") => Command::Radio(on_off(words.next())?),
		Some("pause") => Command::Pause(number(words.next(), 1, 1000)?),
		Some("priority") => Command::Priority(number(words.next(), 0, 3)? as usize),
		Some("reboot") => Command::Reboot,
		Some("ntp") => Command::Ntp,
		Some("wifi") => Command::Wifi(wifi(&mut words)?),
		Some("dns") => {
			let host = words.next().ok_or("expected DNS name")?;
			if host.len() > 253
				|| !host
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
			{
				return Err("invalid DNS name");
			}
			Command::Dns(host)
		}
		Some("tcp") => Command::Tcp(match words.next() {
			None | Some("peer") => None,
			Some(endpoint) => {
				let address: SocketAddrV4 = endpoint.parse().map_err(|_| "expected IPv4:port")?;
				if address.port() == 0 {
					return Err("port must be nonzero");
				}
				Some(address)
			}
		}),
		_ => return Err("unknown command; use diag help"),
	};
	if words.next().is_some() {
		return Err("unexpected argument");
	}
	Ok(command)
}

fn wifi(words: &mut core::str::SplitAsciiWhitespace<'_>) -> Result<WifiCommand, &'static str> {
	Ok(match words.next() {
		None | Some("status") => WifiCommand::Status,
		Some("pause") => WifiCommand::Enabled(false),
		Some("resume") => WifiCommand::Enabled(true),
		Some("reconnect") => WifiCommand::Reconnect,
		Some("reset") => WifiCommand::Reset,
		Some("scan") => {
			let active = match words.next() {
				None | Some("passive") => false,
				Some("active") => true,
				_ => return Err("expected active or passive"),
			};
			let channel = channel(words.next())?;
			let dwell = match words.next() {
				None => 360,
				value => number(value, 10, 1000)?,
			};
			WifiCommand::Scan {
				active,
				channel,
				dwell,
			}
		}
		Some("power") => WifiCommand::Power(number(words.next(), 8, 84)? as i8),
		Some("ps") => WifiCommand::PowerSave(match words.next() {
			Some("none") => PowerSave::None,
			Some("min") => PowerSave::Min,
			Some("max") => PowerSave::Max,
			_ => return Err("expected none, min or max"),
		}),
		Some("protocol") => WifiCommand::Protocols(match words.next() {
			Some("b") => Protocols::B,
			Some("bg") => Protocols::Bg,
			Some("bgn") => Protocols::Bgn,
			Some("bgnax") => Protocols::Bgnax,
			_ => return Err("expected b, bg, bgn or bgnax"),
		}),
		Some("bandwidth") => WifiCommand::Bandwidth(match words.next() {
			Some("20") => 20,
			Some("40") => 40,
			_ => return Err("expected 20 or 40 MHz"),
		}),
		Some("bssid") => WifiCommand::Bssid(match words.next() {
			Some("auto") => None,
			Some(mac) => Some(mac_address(mac)?),
			None => return Err("expected auto or MAC address"),
		}),
		Some("channel") => {
			let value = words.next().ok_or("expected auto or channel")?;
			WifiCommand::Channel(channel(Some(value))?)
		}
		Some("method") => WifiCommand::AllChannels(match words.next() {
			Some("all") => true,
			Some("fast") => false,
			_ => return Err("expected all or fast"),
		}),
		Some("auth") => WifiCommand::MixedAuthentication(match words.next() {
			Some("wpa2") => false,
			Some("mixed") => true,
			_ => return Err("expected wpa2 or mixed (WPA/WPA2)"),
		}),
		Some("sniff") => WifiCommand::Sniff(on_off(words.next())?),
		_ => return Err("unknown WiFi command; use diag help"),
	})
}

fn on_off(value: Option<&str>) -> Result<bool, &'static str> {
	match value {
		Some("on") => Ok(true),
		Some("off") => Ok(false),
		_ => Err("expected on or off"),
	}
}

fn number(value: Option<&str>, min: u16, max: u16) -> Result<u16, &'static str> {
	let value = value
		.ok_or("missing number")?
		.parse()
		.map_err(|_| "invalid number")?;
	if value < min || value > max {
		return Err("number out of range");
	}
	Ok(value)
}

fn channel(value: Option<&str>) -> Result<Option<u8>, &'static str> {
	match value {
		None | Some("auto") => Ok(None),
		value => number(value, 1, 13).map(|value| Some(value as u8)),
	}
}

fn mac_address(text: &str) -> Result<[u8; 6], &'static str> {
	let mut bytes = text.split(':');
	let mut mac = [0; 6];
	for byte in &mut mac {
		let text = bytes.next().ok_or("incomplete MAC address")?;
		if text.len() != 2 {
			return Err("expected two hex digits per MAC byte");
		}
		*byte = u8::from_str_radix(text, 16).map_err(|_| "invalid MAC address")?;
	}
	if bytes.next().is_some() {
		return Err("MAC address too long");
	}
	Ok(mac)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_access_point_selection_and_reset() {
		assert_eq!(
			parse("diag wifi bssid 7c:77:16:11:1e:56\r"),
			Ok(Command::Wifi(WifiCommand::Bssid(Some([
				0x7c, 0x77, 0x16, 0x11, 0x1e, 0x56
			]))))
		);
		assert_eq!(
			parse("diag wifi bssid auto"),
			Ok(Command::Wifi(WifiCommand::Bssid(None)))
		);
		assert_eq!(
			parse("diag wifi reset"),
			Ok(Command::Wifi(WifiCommand::Reset))
		);
	}

	#[test]
	fn parses_scan_timing_and_defaults() {
		assert_eq!(
			parse("diag wifi scan active 1 300"),
			Ok(Command::Wifi(WifiCommand::Scan {
				active: true,
				channel: Some(1),
				dwell: 300
			}))
		);
		assert_eq!(
			parse("diag wifi scan"),
			Ok(Command::Wifi(WifiCommand::Scan {
				active: false,
				channel: None,
				dwell: 360
			}))
		);
	}

	#[test]
	fn rejects_dangerous_or_malformed_arguments() {
		for text in [
			"diag wifi power 85",
			"diag wifi power -1",
			"diag wifi power 7",
			"diag wifi channel 14",
			"diag pause 60000",
			"diag priority 31",
			"diag wifi scan active 1 0",
			"diag wifi bssid 1:2:3:4:5:6",
			"diag wifi bssid 00:11:22:33:44:55:66",
			"diag reboot extra",
			"diag tcp 192.168.1.1:0",
			"diag dns a/b",
		] {
			assert!(parse(text).is_err(), "{text}");
		}
	}

	#[test]
	fn parses_peer_and_explicit_tcp_targets() {
		assert_eq!(parse("diag tcp peer"), Ok(Command::Tcp(None)));
		assert_eq!(
			parse("diag tcp 192.168.1.176:9001"),
			Ok(Command::Tcp(Some("192.168.1.176:9001".parse().unwrap())))
		);
	}
}
