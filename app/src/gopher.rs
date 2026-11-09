//! Gopher (RFC 1436): menus, addresses and the board's own menu.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

pub const PORT: u16 = 70;

/// One line of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
	/// `0` text, `1` menu, `7` search, `i` information, `3` error, and others.
	pub kind: char,
	pub text: String,
	pub selector: String,
	pub host: String,
	pub port: u16,
}

impl Item {
	pub fn is_link(&self) -> bool {
		!matches!(self.kind, 'i' | '3')
	}

	/// Whether the client can open this item.
	pub fn is_supported(&self) -> bool {
		matches!(self.kind, '0' | '1' | '7')
	}

	pub fn url(&self) -> Url {
		Url {
			host: self.host.clone(),
			port: self.port,
			kind: self.kind,
			selector: self.selector.clone(),
		}
	}
}

/// What to fetch, and how to show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
	pub host: String,
	pub port: u16,
	pub kind: char,
	pub selector: String,
}

impl Url {
	/// Reads `gopher://host:port/1/selector`, `[address]:port`, or a bare
	/// address, which opens its root menu.
	pub fn parse(text: &str) -> Option<Url> {
		let text = text.trim();
		let text = text.strip_prefix("gopher://").unwrap_or(text);
		let (authority, path) = match text.find('/') {
			// A slash inside brackets is not part of an address, so look after them.
			Some(_) if text.starts_with('[') => {
				let close = text.find(']')?;
				match text[close..].find('/') {
					Some(slash) => text.split_at(close + slash),
					None => (text, ""),
				}
			}
			Some(slash) => text.split_at(slash),
			None => (text, ""),
		};
		let (host, port) = split_host_port(authority)?;
		let mut path = path.strip_prefix('/').unwrap_or(path).chars();
		let kind = path.next().unwrap_or('1');
		Some(Url {
			host,
			port,
			kind,
			selector: path.collect(),
		})
	}

	/// The host, if it is an IPv6 address rather than a name.
	pub fn address(&self) -> Option<Ipv6Addr> {
		self.host.parse().ok()
	}

	pub fn request(&self, query: Option<&str>) -> Vec<u8> {
		let mut line = self.selector.clone();
		if let Some(query) = query {
			line.push('\t');
			line.push_str(query);
		}
		line.push_str("\r\n");
		line.into_bytes()
	}
}

impl core::fmt::Display for Url {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		let host = if self.host.contains(':') {
			format!("[{}]", self.host)
		} else {
			self.host.clone()
		};
		write!(f, "gopher://{host}")?;
		if self.port != PORT {
			write!(f, ":{}", self.port)?;
		}
		write!(f, "/{}{}", self.kind, self.selector)
	}
}

fn split_host_port(authority: &str) -> Option<(String, u16)> {
	if let Some(rest) = authority.strip_prefix('[') {
		let (host, after) = rest.split_once(']')?;
		let port = match after.strip_prefix(':') {
			Some(port) => port.parse().ok()?,
			None if after.is_empty() => PORT,
			None => return None,
		};
		return Some((host.into(), port));
	}
	// A bare IPv6 address has several colons and no port.
	if authority.matches(':').count() > 1 {
		return Some((authority.into(), PORT));
	}
	match authority.split_once(':') {
		Some((host, port)) => Some((host.into(), port.parse().ok()?)),
		None if !authority.is_empty() => Some((authority.into(), PORT)),
		None => None,
	}
}

pub fn parse_menu(data: &[u8]) -> Vec<Item> {
	let text = String::from_utf8_lossy(data);
	text.lines()
		.take_while(|line| *line != ".")
		.filter(|line| !line.is_empty())
		.map(|line| {
			let mut chars = line.chars();
			let kind = chars.next().unwrap_or('i');
			let mut fields = chars.as_str().split('\t');
			let text = fields.next().unwrap_or_default().into();
			let selector = fields.next().unwrap_or_default().into();
			let host = fields.next().unwrap_or_default().into();
			let port = fields
				.next()
				.and_then(|port| port.trim().parse().ok())
				.unwrap_or(PORT);
			Item {
				kind,
				text,
				selector,
				host,
				port,
			}
		})
		.collect()
}

/// A text document, without the line that ends it.
pub fn parse_text(data: &[u8]) -> String {
	let text = String::from_utf8_lossy(data);
	let mut lines: Vec<&str> = text.lines().collect();
	if lines.last() == Some(&".") {
		lines.pop();
	}
	lines.join("\n")
}

/// Builds a menu line for the board's own menu.
pub fn menu_line(kind: char, text: &str, selector: &str, host: &str, port: u16) -> String {
	format!("{kind}{text}\t{selector}\t{host}\t{port}\r\n")
}

pub fn info_line(text: &str) -> String {
	menu_line('i', text, "", "error.host", 1)
}

/// The board's menu: what it is, and links to its peers' menus.
pub fn status_menu(lines: &[String], peers: &[Ipv6Addr]) -> Vec<u8> {
	let mut menu = String::new();
	for line in lines {
		menu.push_str(&info_line(line));
	}
	if !peers.is_empty() {
		menu.push_str(&info_line(""));
		menu.push_str(&info_line("Peers:"));
		for peer in peers {
			menu.push_str(&menu_line(
				'1',
				&peer.to_string(),
				"",
				&peer.to_string(),
				PORT,
			));
		}
	}
	menu.push_str(".\r\n");
	menu.into_bytes()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_urls() {
		let url = Url::parse("gopher://[200::1]/1/phlog").unwrap();
		assert_eq!(
			url,
			Url {
				host: "200::1".into(),
				port: 70,
				kind: '1',
				selector: "/phlog".into()
			}
		);
		assert_eq!(url.to_string(), "gopher://[200::1]/1/phlog");
		let url = Url::parse("[200::1]:7070/0/notes.txt").unwrap();
		assert_eq!(
			(url.port, url.kind, url.selector.as_str()),
			(7070, '0', "/notes.txt")
		);
		let url = Url::parse("200:a59::9ac1").unwrap();
		assert_eq!(
			(url.host.as_str(), url.port, url.kind),
			("200:a59::9ac1", 70, '1')
		);
		assert_eq!(
			Url::parse("gopher.example:70").unwrap().host,
			"gopher.example"
		);
		assert_eq!(Url::parse(""), None);
	}

	#[test]
	fn reads_menus() {
		let menu = b"iWelcome\t\terror.host\t1\r\n1Phlog\t/phlog\t200::1\t70\r\n0About\t/about.txt\t200::1\t70\r\n.\r\n";
		let items = parse_menu(menu);
		assert_eq!(items.len(), 3);
		assert!(!items[0].is_link());
		assert_eq!(items[1].url().to_string(), "gopher://[200::1]/1/phlog");
		assert_eq!(items[2].kind, '0');
	}

	#[test]
	fn reads_text() {
		assert_eq!(
			parse_text(b"line one\r\nline two\r\n.\r\n"),
			"line one\nline two"
		);
	}

	#[test]
	fn builds_a_menu_that_reads_back() {
		let peer: Ipv6Addr = "200::2".parse().unwrap();
		let menu = status_menu(&["kitchen".into()], &[peer]);
		let items = parse_menu(&menu);
		assert_eq!(items[0].text, "kitchen");
		let link = items.iter().find(|item| item.is_link()).unwrap();
		assert_eq!(link.url().address(), Some(peer));
	}
}
