//! A tiny HTTP/1.0 server for the board's status page.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::net::Ipv6Addr;
use core::time::Duration;

use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;
use yggdrasil_espnow::{Node, Platform};

/// Requests longer than this are refused.
pub const MAX_REQUEST: usize = 2048;

/// Facts about the board that the node does not know.
#[derive(Clone, Debug, Default)]
pub struct BoardInfo {
	/// What the user called the board.
	pub name: String,
	/// The kind of board, such as "M5Stack Cardputer".
	pub model: String,
	pub uptime: Duration,
	pub free_heap: Option<usize>,
	/// Microseconds since the Unix epoch, once the board knows the time.
	pub time: Option<u64>,
}

/// Whether `request` holds a complete header.
pub fn is_complete(request: &[u8]) -> bool {
	request.windows(4).any(|window| window == b"\r\n\r\n") || request.len() >= MAX_REQUEST
}

/// The full response to `request`.
pub fn respond<P: Platform>(request: &[u8], node: &Node<P>, board: &BoardInfo) -> Vec<u8> {
	let line = request
		.split(|&byte| byte == b'\n')
		.next()
		.unwrap_or_default();
	let mut parts = line.split(|&byte| byte == b' ');
	let method = parts.next().unwrap_or_default();
	let path = parts.next().unwrap_or_default();
	match (method, path) {
		(b"GET", b"/") => response(
			"200 OK",
			"text/html; charset=utf-8",
			&status_page(node, board),
		),
		(b"GET", _) => response("404 Not Found", "text/plain", "Not found\n"),
		_ => response(
			"405 Method Not Allowed",
			"text/plain",
			"Only GET is supported\n",
		),
	}
}

fn response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
	let mut response = format!(
		"HTTP/1.0 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
		body.len()
	);
	response.push_str(body);
	response.into_bytes()
}

fn status_page<P: Platform>(node: &Node<P>, board: &BoardInfo) -> String {
	let title = if board.name.is_empty() {
		String::from("esp-ygg")
	} else {
		escape(&board.name)
	};
	let mut page = format!(
		"<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
		<meta name=\"viewport\" content=\"width=device-width\">\
		<title>{title}</title>\
		<style>body{{font-family:system-ui,sans-serif;max-width:40em;margin:2em auto;padding:0 1em}}\
		td{{padding:.2em 1em .2em 0}}code{{font-size:.9em}}</style></head><body>\n<h1>{title}</h1>\n<table>\n"
	);
	let address = Ipv6Addr::from(node.address().0);
	let mut subnet = [0u8; 16];
	subnet[..8].copy_from_slice(&node.subnet().0);
	row(&mut page, "Address", &format!("<code>{address}</code>"));
	row(
		&mut page,
		"Subnet",
		&format!("<code>{}/64</code>", Ipv6Addr::from(subnet)),
	);
	row(
		&mut page,
		"Public key",
		&format!("<code>{}</code>", Hex(&node.public_key().0)),
	);
	if !board.model.is_empty() {
		row(&mut page, "Board", &escape(&board.model));
	}
	if let Some(time) = board.time {
		let now = crate::time::DateTime::from_unix((time / 1_000_000) as i64, 0);
		row(&mut page, "Time", &format!("{} UTC", now.date_and_clock()));
	}
	row(&mut page, "Uptime", &uptime(board.uptime));
	if let Some(free) = board.free_heap {
		row(&mut page, "Free memory", &format!("{} KB", free / 1024));
	}
	row(
		&mut page,
		"Known nodes",
		&format!("{}", node.network().debug().get_tree().len()),
	);
	page.push_str("</table>\n<h2>Peers</h2>\n");
	let peers = node.network().debug().get_peers();
	if peers.is_empty() {
		page.push_str("<p>None</p>\n");
	} else {
		page.push_str("<table>\n");
		for peer in peers {
			let address = Ipv6Addr::from(addr_for_key(&peer.key).0);
			let latency = format!("{} ms", peer.latency.as_millis());
			row(&mut page, &format!("<code>{address}</code>"), &latency);
		}
		page.push_str("</table>\n");
	}
	page.push_str("</body></html>\n");
	page
}

/// The board's facts as plain lines, for the Gopher menu.
pub fn summary<P: Platform>(node: &Node<P>, board: &BoardInfo) -> Vec<String> {
	let mut lines = Vec::new();
	lines.push(if board.name.is_empty() {
		String::from("esp-ygg")
	} else {
		board.name.clone()
	});
	lines.push(String::new());
	lines.push(format!("Address  {}", Ipv6Addr::from(node.address().0)));
	if !board.model.is_empty() {
		lines.push(format!("Board    {}", board.model));
	}
	if let Some(time) = board.time {
		let now = crate::time::DateTime::from_unix((time / 1_000_000) as i64, 0);
		lines.push(format!("Time     {} UTC", now.date_and_clock()));
	}
	lines.push(format!("Uptime   {}", uptime(board.uptime)));
	if let Some(free) = board.free_heap {
		lines.push(format!("Memory   {} KB free", free / 1024));
	}
	lines.push(format!(
		"Known    {} nodes",
		node.network().debug().get_tree().len()
	));
	lines
}

fn row(page: &mut String, label: &str, value: &str) {
	let _ = writeln!(page, "<tr><td>{label}</td><td>{value}</td></tr>");
}

fn uptime(uptime: Duration) -> String {
	let seconds = uptime.as_secs();
	let (days, hours, minutes) = (seconds / 86_400, seconds / 3_600 % 24, seconds / 60 % 60);
	if days > 0 {
		format!("{days} d {hours} h {minutes} min")
	} else if hours > 0 {
		format!("{hours} h {minutes} min")
	} else {
		format!("{minutes} min {} s", seconds % 60)
	}
}

/// Makes user-chosen text safe to put in HTML.
fn escape(text: &str) -> String {
	let mut escaped = String::with_capacity(text.len());
	for character in text.chars() {
		match character {
			'&' => escaped.push_str("&amp;"),
			'<' => escaped.push_str("&lt;"),
			'>' => escaped.push_str("&gt;"),
			'"' => escaped.push_str("&quot;"),
			'\'' => escaped.push_str("&#39;"),
			_ => escaped.push(character),
		}
	}
	escaped
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn escapes_markup() {
		assert_eq!(
			escape("<b>Tom & \"Jerry\"</b>"),
			"&lt;b&gt;Tom &amp; &quot;Jerry&quot;&lt;/b&gt;"
		);
	}

	#[test]
	fn formats_uptime() {
		assert_eq!(uptime(Duration::from_secs(75)), "1 min 15 s");
		assert_eq!(uptime(Duration::from_secs(3 * 3600 + 120)), "3 h 2 min");
		assert_eq!(
			uptime(Duration::from_secs(2 * 86_400 + 3600)),
			"2 d 1 h 0 min"
		);
	}
}
