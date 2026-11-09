//! Sends ESP-NOW frames from their own task, so a slow radio holds up only
//! other frames, never the node's timers or its TCP peering.

use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, with_timeout};
use esp_radio::esp_now::EspNowSender;
use log::warn;
use yggdrasil_espnow::Mac;

/// How long to wait for the radio to report on a frame before moving on.
const SEND_TIMEOUT: Duration = Duration::from_millis(100);
const REPORT_PERIOD: Duration = Duration::from_secs(10);

pub struct Outgoing {
	pub mac: Mac,
	pub frame: Vec<u8>,
}

pub static OUTBOX: Channel<CriticalSectionRawMutex, Outgoing, 16> = Channel::new();

#[embassy_executor::task]
pub async fn sender(mut sender: EspNowSender) {
	let mut stats = SendStats::new();
	loop {
		let outgoing = OUTBOX.receive().await;
		// Lost frames are resent by the link layer, so failures are only counted.
		let send = sender.send_async(&outgoing.mac, &outgoing.frame);
		match with_timeout(SEND_TIMEOUT, send).await {
			Ok(Ok(())) => {}
			// Broadcasts are never acknowledged, so they never fail.
			Ok(Err(_)) => stats.failed += 1,
			Err(_) => stats.timed_out += 1,
		}
		stats.sent += 1;
		stats.report_if_due();
	}
}

/// Counts sends that went unacknowledged or never finished, and logs them
/// every so often.
struct SendStats {
	sent: u32,
	failed: u32,
	timed_out: u32,
	since: Instant,
}

impl SendStats {
	fn new() -> Self {
		SendStats {
			sent: 0,
			failed: 0,
			timed_out: 0,
			since: Instant::now(),
		}
	}

	fn report_if_due(&mut self) {
		if Instant::now() < self.since + REPORT_PERIOD {
			return;
		}
		if self.failed > 0 || self.timed_out > 0 {
			warn!(
				"ESP-NOW sends in the last {} s: {} sent, {} unacknowledged, {} timed out",
				(Instant::now() - self.since).as_secs(),
				self.sent,
				self.failed,
				self.timed_out
			);
		}
		*self = SendStats::new();
	}
}
