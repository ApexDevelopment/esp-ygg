//! IRC messages (RFC 2812): reading lines from a server and writing commands.

use alloc::string::String;
use alloc::vec::Vec;

pub const PORT: u16 = 6667;
/// The longest line a server accepts, with its ending.
pub const MAX_LINE: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
	/// Who sent it: a nick, `nick!user@host`, or a server name.
	pub prefix: Option<String>,
	pub command: String,
	/// The parameters, with the trailing one last.
	pub params: Vec<String>,
}

impl Message {
	pub fn parse(line: &str) -> Option<Message> {
		let line = line.trim_end_matches(['\r', '\n']);
		let (prefix, rest) = match line.strip_prefix(':') {
			Some(rest) => {
				let (prefix, rest) = rest.split_once(' ')?;
				(Some(prefix.into()), rest)
			}
			None => (None, line),
		};
		let (middle, trailing) = match rest.split_once(" :") {
			Some((middle, trailing)) => (middle, Some(trailing)),
			None => (rest, None),
		};
		let mut words = middle.split(' ').filter(|word| !word.is_empty());
		let command = words.next()?.to_ascii_uppercase();
		let mut params: Vec<String> = words.map(String::from).collect();
		params.extend(trailing.map(String::from));
		Some(Message {
			prefix,
			command,
			params,
		})
	}

	/// The sender's nick, without the user and host.
	pub fn nick(&self) -> Option<&str> {
		let prefix = self.prefix.as_deref()?;
		Some(prefix.split_once('!').map_or(prefix, |(nick, _)| nick))
	}

	pub fn param(&self, index: usize) -> &str {
		self.params.get(index).map_or("", String::as_str)
	}

	/// The last parameter, which usually holds the text.
	pub fn text(&self) -> &str {
		self.params.last().map_or("", String::as_str)
	}
}

/// A command line, with the last parameter allowed to hold spaces.
pub fn command(command: &str, params: &[&str], trailing: Option<&str>) -> Vec<u8> {
	let mut line = String::from(command);
	for param in params {
		line.push(' ');
		line.push_str(param);
	}
	if let Some(trailing) = trailing {
		line.push_str(" :");
		line.push_str(trailing);
	}
	// Nothing may smuggle in another command.
	line.retain(|character| character != '\r' && character != '\n');
	line.truncate(MAX_LINE - 2);
	line.push_str("\r\n");
	line.into_bytes()
}

/// A `/me` action, sent as a CTCP ACTION.
pub fn action(target: &str, text: &str) -> Vec<u8> {
	let mut body = String::from("\u{1}ACTION ");
	body.push_str(text);
	body.push('\u{1}');
	command("PRIVMSG", &[target], Some(&body))
}

/// The text of a CTCP ACTION, if `text` is one.
pub fn action_text(text: &str) -> Option<&str> {
	text.strip_prefix("\u{1}ACTION ")
		.map(|rest| rest.trim_end_matches('\u{1}'))
}

pub fn is_channel(target: &str) -> bool {
	target.starts_with(['#', '&', '+', '!'])
}

/// Splits received bytes into complete lines, keeping a partial one for later.
#[derive(Default)]
pub struct LineReader {
	partial: Vec<u8>,
}

impl LineReader {
	pub fn push(&mut self, data: &[u8]) -> Vec<String> {
		self.partial.extend_from_slice(data);
		let mut lines = Vec::new();
		while let Some(end) = self.partial.iter().position(|&byte| byte == b'\n') {
			let line: Vec<u8> = self.partial.drain(..=end).collect();
			lines.push(String::from_utf8_lossy(&line).trim_end().into());
		}
		// A line this long without an ending is not IRC.
		if self.partial.len() > MAX_LINE * 2 {
			self.partial.clear();
		}
		lines
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_messages() {
		let message = Message::parse(":ada!ada@200::1 PRIVMSG #ygg :hello there\r\n").unwrap();
		assert_eq!(message.nick(), Some("ada"));
		assert_eq!(message.command, "PRIVMSG");
		assert_eq!(message.params, ["#ygg", "hello there"]);
		let ping = Message::parse("PING :irc.example").unwrap();
		assert_eq!(
			(ping.prefix.as_deref(), ping.command.as_str(), ping.text()),
			(None, "PING", "irc.example")
		);
		let welcome = Message::parse(":irc.example 001 board :Welcome").unwrap();
		assert_eq!(
			(welcome.nick(), welcome.param(0)),
			(Some("irc.example"), "board")
		);
		assert_eq!(Message::parse(""), None);
	}

	#[test]
	fn writes_commands_without_line_breaks() {
		assert_eq!(command("JOIN", &["#ygg"], None), b"JOIN #ygg\r\n");
		assert_eq!(
			command("PRIVMSG", &["#ygg"], Some("hi\r\nQUIT")),
			b"PRIVMSG #ygg :hiQUIT\r\n"
		);
	}

	#[test]
	fn round_trips_actions() {
		let line = String::from_utf8(action("#ygg", "waves")).unwrap();
		let message = Message::parse(&line).unwrap();
		assert_eq!(action_text(message.text()), Some("waves"));
	}

	#[test]
	fn reassembles_lines() {
		let mut reader = LineReader::default();
		assert_eq!(reader.push(b"PING :a\r\nPRIV"), ["PING :a"]);
		assert_eq!(reader.push(b"MSG #x :y\r\n"), ["PRIVMSG #x :y"]);
	}
}
