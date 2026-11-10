//! Gemini: addresses, requests, response headers and gemtext.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

pub const PORT: u16 = 1965;
/// The longest request a server must accept, without its line ending.
const MAX_URL: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
	pub host: String,
	pub port: u16,
	/// Starts with `/`, and may hold a `?query`.
	pub path: String,
}

impl Url {
	/// Reads `gemini://host:port/path`, `[address]:port/path`, or a bare
	/// address, which opens its root page.
	pub fn parse(text: &str) -> Option<Url> {
		let text = text.trim();
		let text = match text.split_once("://") {
			Some((scheme, rest)) if scheme.eq_ignore_ascii_case("gemini") => rest,
			Some(_) => return None,
			None => text,
		};
		let (authority, path) = split_authority(text);
		let (host, port) = crate::authority::split_host_port(authority, PORT)?;
		let path = if path.is_empty() { "/" } else { path };
		Some(Url {
			host,
			port,
			path: remove_dot_segments(path),
		})
	}

	/// The host, if it is an IPv6 address rather than a name.
	pub fn address(&self) -> Option<Ipv6Addr> {
		self.host.parse().ok()
	}

	/// Where a link on this page leads.
	pub fn join(&self, link: &str) -> Result<Url, LinkError> {
		let link = link.trim();
		if let Some((scheme, _)) = link.split_once(':')
			&& !scheme.is_empty()
			&& scheme
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
			&& !scheme.contains(['/', '?'])
			&& !is_address(link)
		{
			if scheme.eq_ignore_ascii_case("gemini") {
				return Url::parse(link).ok_or(LinkError::Invalid);
			}
			return Err(LinkError::OtherScheme(scheme.to_ascii_lowercase()));
		}
		if let Some(rest) = link.strip_prefix("//") {
			return Url::parse(rest).ok_or(LinkError::Invalid);
		}
		let path = if link.is_empty() {
			self.path.clone()
		} else if link.starts_with('/') {
			link.into()
		} else if link.starts_with('?') {
			let (path, _) = self.path.split_once('?').unwrap_or((&self.path, ""));
			format!("{path}{link}")
		} else {
			let (path, _) = self.path.split_once('?').unwrap_or((&self.path, ""));
			let directory = &path[..path.rfind('/').map_or(0, |slash| slash + 1)];
			format!("{directory}{link}")
		};
		Ok(Url {
			host: self.host.clone(),
			port: self.port,
			path: remove_dot_segments(&path),
		})
	}

	/// This page, asked with `query`.
	pub fn with_query(&self, query: &str) -> Url {
		let (path, _) = self.path.split_once('?').unwrap_or((&self.path, ""));
		Url {
			host: self.host.clone(),
			port: self.port,
			path: format!("{path}?{}", percent_encode(query)),
		}
	}

	pub fn request(&self) -> Option<Vec<u8>> {
		let url = self.to_string();
		if url.len() > MAX_URL {
			return None;
		}
		Some(format!("{url}\r\n").into_bytes())
	}
}

impl core::fmt::Display for Url {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		if self.host.contains(':') {
			write!(f, "gemini://[{}]", self.host)?;
		} else {
			write!(f, "gemini://{}", self.host)?;
		}
		if self.port != PORT {
			write!(f, ":{}", self.port)?;
		}
		f.write_str(&self.path)
	}
}

/// A bare IPv6 address has colons but no scheme.
fn is_address(link: &str) -> bool {
	link.split(['/', '?'])
		.next()
		.is_some_and(|host| host.parse::<Ipv6Addr>().is_ok())
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkError {
	Invalid,
	/// A link to another protocol, such as `https`.
	OtherScheme(String),
}

fn split_authority(text: &str) -> (&str, &str) {
	// A slash inside brackets is not part of an address, so look after them.
	let search_from = if text.starts_with('[') {
		text.find(']').unwrap_or(0)
	} else {
		0
	};
	match text[search_from..].find(['/', '?']) {
		Some(index) => text.split_at(search_from + index),
		None => (text, ""),
	}
}

/// Resolves `.` and `..` in a path, as RFC 3986 does.
fn remove_dot_segments(path: &str) -> String {
	let (path, query) = match path.split_once('?') {
		Some((path, query)) => (path, Some(query)),
		None => (path, None),
	};
	let mut segments: Vec<&str> = Vec::new();
	let mut parts = path.split('/').skip(1).peekable();
	while let Some(part) = parts.next() {
		let last = parts.peek().is_none();
		match part {
			"." | ".." => {
				if part == ".." {
					segments.pop();
				}
				// A path ending in a dot segment still names a directory.
				if last {
					segments.push("");
				}
			}
			part => segments.push(part),
		}
	}
	let mut resolved = String::from("/");
	resolved.push_str(&segments.join("/"));
	if let Some(query) = query {
		resolved.push('?');
		resolved.push_str(query);
	}
	resolved
}

fn percent_encode(text: &str) -> String {
	let mut encoded = String::with_capacity(text.len());
	for byte in text.bytes() {
		if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
			encoded.push(byte as char);
		} else {
			encoded.push_str(&format!("%{byte:02X}"));
		}
	}
	encoded
}

/// The first line of a response.
#[derive(Debug, PartialEq, Eq)]
pub struct Header {
	pub status: u8,
	/// The prompt, media type, redirect target or error message, by status.
	pub meta: String,
}

impl Header {
	/// The header and the body after it, once the whole header has arrived.
	pub fn parse(response: &[u8]) -> Option<(Header, &[u8])> {
		let end = response.windows(2).position(|pair| pair == b"\r\n")?;
		let line = core::str::from_utf8(&response[..end]).ok()?;
		let (status, meta) = line.split_once(' ').unwrap_or((line, ""));
		if status.len() != 2 {
			return None;
		}
		let header = Header {
			status: status.parse().ok()?,
			meta: meta.trim().into(),
		};
		Some((header, &response[end + 2..]))
	}
}

/// One line of a gemtext page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
	Text(String),
	Link { url: String, label: String },
	Heading(String),
	Item(String),
	Quote(String),
	Preformatted(String),
}

pub fn parse_gemtext(text: &str) -> Vec<Line> {
	let mut lines = Vec::new();
	let mut preformatted = false;
	for line in text.lines() {
		if line.starts_with("```") {
			preformatted = !preformatted;
			continue;
		}
		if preformatted {
			lines.push(Line::Preformatted(line.into()));
			continue;
		}
		let parsed = if let Some(rest) = line.strip_prefix("=>") {
			let rest = rest.trim_start();
			let (url, label) = rest
				.split_once(char::is_whitespace)
				.map_or((rest, ""), |(url, label)| (url, label.trim()));
			if url.is_empty() {
				Line::Text(line.into())
			} else {
				Line::Link {
					url: url.into(),
					label: if label.is_empty() { url } else { label }.into(),
				}
			}
		} else if line.starts_with('#') {
			Line::Heading(line.trim_start_matches('#').trim().into())
		} else if let Some(rest) = line.strip_prefix("* ") {
			Line::Item(rest.into())
		} else if let Some(rest) = line.strip_prefix('>') {
			Line::Quote(rest.trim_start().into())
		} else {
			Line::Text(line.into())
		};
		lines.push(parsed);
	}
	lines
}

/// What the status of a response asks for, in words, for errors.
pub fn describe_failure(header: &Header) -> String {
	let kind = match header.status / 10 {
		4 => "Temporary failure",
		5 => "Permanent failure",
		6 => "Needs a client certificate",
		_ => "Unexpected response",
	};
	if header.meta.is_empty() {
		format!("{kind} ({})", header.status)
	} else {
		format!("{} {}", header.status, header.meta)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn url(text: &str) -> Url {
		Url::parse(text).unwrap()
	}

	#[test]
	fn parses_urls() {
		let parsed = url("gemini://[200::1]:1966/docs/index.gmi");
		assert_eq!(
			(parsed.host.as_str(), parsed.port, parsed.path.as_str()),
			("200::1", 1966, "/docs/index.gmi")
		);
		assert_eq!(parsed.to_string(), "gemini://[200::1]:1966/docs/index.gmi");
		assert_eq!(
			url("200:a59::9ac1").to_string(),
			"gemini://[200:a59::9ac1]/"
		);
		assert_eq!(url("example.org?q").path, "/?q");
		assert_eq!(Url::parse("https://example.org"), None);
	}

	#[test]
	fn resolves_links() {
		let base = url("gemini://[200::1]/a/b/page.gmi?x");
		let join = |link| base.join(link).map(|url| url.to_string());
		assert_eq!(
			join("other.gmi"),
			Ok("gemini://[200::1]/a/b/other.gmi".into())
		);
		assert_eq!(join("../up/"), Ok("gemini://[200::1]/a/up/".into()));
		assert_eq!(join(".."), Ok("gemini://[200::1]/a/".into()));
		assert_eq!(join("/top"), Ok("gemini://[200::1]/top".into()));
		assert_eq!(join("?y"), Ok("gemini://[200::1]/a/b/page.gmi?y".into()));
		assert_eq!(join("//[200::2]/"), Ok("gemini://[200::2]/".into()));
		assert_eq!(
			join("gemini://[200::3]/x"),
			Ok("gemini://[200::3]/x".into())
		);
		assert_eq!(
			join("https://example.org"),
			Err(LinkError::OtherScheme("https".into()))
		);
	}

	#[test]
	fn encodes_queries() {
		let page = url("gemini://[200::1]/search?old");
		assert_eq!(page.with_query("a b&c").path, "/search?a%20b%26c");
	}

	#[test]
	fn reads_headers() {
		let (header, body) = Header::parse(b"20 text/gemini; lang=en\r\n# Hi\n").unwrap();
		assert_eq!(header.status, 20);
		assert_eq!(header.meta, "text/gemini; lang=en");
		assert_eq!(body, b"# Hi\n");
		assert_eq!(Header::parse(b"20 text/gem"), None);
		assert_eq!(Header::parse(b"200 nope\r\n"), None);
	}

	#[test]
	fn reads_gemtext() {
		let page = "# Title\nSome text\n=> /next Next page\n=>/bare\n* item\n> quote\n```art\n=> not a link\n```\n";
		assert_eq!(
			parse_gemtext(page),
			[
				Line::Heading("Title".into()),
				Line::Text("Some text".into()),
				Line::Link {
					url: "/next".into(),
					label: "Next page".into()
				},
				Line::Link {
					url: "/bare".into(),
					label: "/bare".into()
				},
				Line::Item("item".into()),
				Line::Quote("quote".into()),
				Line::Preformatted("=> not a link".into()),
			]
		);
	}
}
