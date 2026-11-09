//! Sends ESP-NOW frames from their own task, so a slow radio holds up only
//! other frames, never the node's timers or its TCP peering.

use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, with_timeout};
use esp_radio::esp_now::{EspNowError, EspNowSender};
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
/// Unicast frames that did not get through, for the node to resend sooner.
pub static LOST: Channel<CriticalSectionRawMutex, Outgoing, 8> = Channel::new();

#[embassy_executor::task]
pub async fn sender(mut sender: EspNowSender) {
	let mut stats = SendStats::new();
	loop {
		let outgoing = OUTBOX.receive().await;
		let send = sender.send_async(&outgoing.mac, &outgoing.frame);
		let lost = match with_timeout(SEND_TIMEOUT, send).await {
			Ok(Ok(())) => false,
			// Broadcasts are never acknowledged, so they never fail this way.
			Ok(Err(EspNowError::SendFailed)) => {
				stats.unacknowledged += 1;
				true
			}
			Ok(Err(_)) => {
				stats.refused += 1;
				true
			}
			// It may yet arrive, so leave it to the link's timer.
			Err(_) => {
				stats.timed_out += 1;
				false
			}
		};
		stats.sent += 1;
		stats.min_free_heap = stats.min_free_heap.min(esp_alloc::HEAP.free());
		stats.report_if_due();
		if lost {
			// If the node is behind, its timer will resend the frame anyway.
			let _ = LOST.try_send(outgoing);
		}
	}
}

/// Counts sends that the radio refused, that went unacknowledged, or that
/// never finished, and logs them every so often with the lowest free heap seen.
struct SendStats {
	sent: u32,
	refused: u32,
	unacknowledged: u32,
	timed_out: u32,
	min_free_heap: usize,
	since: Instant,
}

impl SendStats {
	fn new() -> Self {
		SendStats {
			sent: 0,
			refused: 0,
			unacknowledged: 0,
			timed_out: 0,
			min_free_heap: usize::MAX,
			since: Instant::now(),
		}
	}

	fn report_if_due(&mut self) {
		if Instant::now() < self.since + REPORT_PERIOD {
			return;
		}
		if self.refused > 0 || self.unacknowledged > 0 || self.timed_out > 0 {
			warn!(
				"ESP-NOW sends in the last {} s: {} sent, {} refused, {} unacknowledged, {} timed out; lowest free heap {} KB",
				(Instant::now() - self.since).as_secs(),
				self.sent,
				self.refused,
				self.unacknowledged,
				self.timed_out,
				self.min_free_heap / 1024
			);
		}
		*self = SendStats::new();
	}
}
