//! The host and port part of a URL, shared by the Gopher and Gemini clients.

use alloc::string::String;

/// Splits `host`, `host:port`, `[v6]`, `[v6]:port` or a bare IPv6 address,
/// using `default_port` when none is given.
pub fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
	if let Some(rest) = authority.strip_prefix('[') {
		let (host, after) = rest.split_once(']')?;
		let port = match after.strip_prefix(':') {
			Some(port) => port.parse().ok()?,
			None if after.is_empty() => default_port,
			None => return None,
		};
		return Some((host.into(), port));
	}
	// A bare IPv6 address has several colons and no port.
	if authority.matches(':').count() > 1 {
		return Some((authority.into(), default_port));
	}
	match authority.split_once(':') {
		Some((host, port)) if !host.is_empty() => Some((host.into(), port.parse().ok()?)),
		None if !authority.is_empty() => Some((authority.into(), default_port)),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn splits_every_form() {
		let split = |text| split_host_port(text, 70);
		assert_eq!(split("example.org"), Some(("example.org".into(), 70)));
		assert_eq!(
			split("example.org:7070"),
			Some(("example.org".into(), 7070))
		);
		assert_eq!(split("[200::1]:71"), Some(("200::1".into(), 71)));
		assert_eq!(split("[200::1]"), Some(("200::1".into(), 70)));
		assert_eq!(split("200::1"), Some(("200::1".into(), 70)));
		assert_eq!(split(":70"), None);
		assert_eq!(split(""), None);
		assert_eq!(split("[200::1]x"), None);
	}
}
