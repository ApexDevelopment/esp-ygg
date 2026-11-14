//! Ping results shared by the text and graphical interfaces.

use core::net::Ipv6Addr;
use core::time::Duration;

#[derive(Default)]
pub struct Session {
	pub target: Option<Ipv6Addr>,
	pub sent: u32,
	pub received: u32,
	total: Duration,
}

impl Session {
	pub fn start(&mut self, target: Ipv6Addr) {
		*self = Self {
			target: Some(target),
			..Self::default()
		};
	}

	pub fn record_sent(&mut self) {
		self.sent += 1;
	}

	pub fn replied(&mut self, from: Ipv6Addr, round_trip: Duration) -> bool {
		if self.target != Some(from) {
			return false;
		}
		self.received += 1;
		self.total += round_trip;
		true
	}

	pub fn average(&self) -> Option<Duration> {
		(self.received > 0).then(|| self.total / self.received)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn changing_target_resets_results_and_ignores_other_peers() {
		let first = "200::1".parse().unwrap();
		let second = "200::2".parse().unwrap();
		let mut session = Session::default();
		session.start(first);
		session.record_sent();
		assert!(session.replied(first, Duration::from_millis(40)));
		session.record_sent();
		assert!(session.replied(first, Duration::from_millis(20)));
		assert_eq!(session.average(), Some(Duration::from_millis(30)));
		session.start(second);
		assert!(!session.replied(first, Duration::from_millis(10)));
		assert_eq!(
			(session.sent, session.received, session.average()),
			(0, 0, None)
		);
	}
}
