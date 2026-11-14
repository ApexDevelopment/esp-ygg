//! Sends ESP-NOW frames from their own task, so a slow radio holds up only
//! other frames, never the node's timers or its TCP peering.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, with_timeout};
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowSender};
use yggdrasil_espnow::Mac;

/// How long to wait for the radio to report on a frame before moving on.
const SEND_TIMEOUT: Duration = Duration::from_millis(100);

pub struct Outgoing {
	pub mac: Mac,
	pub frame: Vec<u8>,
}

pub static OUTBOX: Channel<CriticalSectionRawMutex, Outgoing, 16> = Channel::new();
/// Unicast frames that did not get through, for the node to resend sooner.
pub static LOST: Channel<CriticalSectionRawMutex, Outgoing, 8> = Channel::new();

/// How much each new sample moves the running averages.
const SMOOTHING: f32 = 0.125;
/// The most addresses to track, so strangers heard once cannot fill memory.
const MAX_TRACKED: usize = 32;

#[derive(Clone, Copy, Default)]
struct Quality {
	signal: Option<f32>,
	loss: Option<f32>,
}

/// How well the radio reaches each MAC address it has heard from or sent to.
static QUALITY: Mutex<CriticalSectionRawMutex, RefCell<BTreeMap<Mac, Quality>>> =
	Mutex::new(RefCell::new(BTreeMap::new()));

fn update(mac: Mac, change: impl FnOnce(&mut Quality)) {
	QUALITY.lock(|quality| {
		let mut quality = quality.borrow_mut();
		if quality.len() >= MAX_TRACKED && !quality.contains_key(&mac) {
			return;
		}
		change(quality.entry(mac).or_default());
	});
}

fn smooth(average: Option<f32>, sample: f32) -> Option<f32> {
	Some(average.map_or(sample, |average| average + (sample - average) * SMOOTHING))
}

/// Records the signal strength of a frame received from `mac`.
pub fn record_signal(mac: Mac, rssi: i32) {
	update(mac, |entry| {
		entry.signal = smooth(entry.signal, rssi as f32)
	});
}

fn record_send(mac: Mac, delivered: bool) {
	let sample = if delivered { 0.0 } else { 100.0 };
	update(mac, |entry| entry.loss = smooth(entry.loss, sample));
}

/// The recent average signal in dBm and loss in percent for `mac`.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn quality(mac: &Mac) -> (Option<i8>, Option<u8>) {
	QUALITY.lock(|quality| {
		let entry = quality.borrow().get(mac).copied().unwrap_or_default();
		(
			entry.signal.map(|signal| signal as i8),
			entry.loss.map(|loss| loss as u8),
		)
	})
}

/// Forgets every address but `keep`, making room for new neighbors.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn forget_others(keep: &[Mac]) {
	QUALITY.lock(|quality| quality.borrow_mut().retain(|mac, _| keep.contains(mac)));
}

#[embassy_executor::task]
pub async fn sender(mut sender: EspNowSender) {
	loop {
		let outgoing = OUTBOX.receive().await;
		#[cfg(feature = "diagnostics")]
		if !crate::diagnostics::RADIO_TX_ENABLED.load(core::sync::atomic::Ordering::Relaxed) {
			continue;
		}
		let send = sender.send_async(&outgoing.mac, &outgoing.frame);
		let lost = match with_timeout(SEND_TIMEOUT, send).await {
			Ok(Ok(())) => false,
			// Refused, or unacknowledged. Broadcasts are never acknowledged,
			// so they never fail that way.
			Ok(Err(_)) => true,
			// It may yet arrive, so leave it to the link's timer.
			Err(_) => false,
		};
		if outgoing.mac != BROADCAST_ADDRESS {
			record_send(outgoing.mac, !lost);
		}
		if lost {
			// If the node is behind, its timer will resend the frame anyway.
			let _ = LOST.try_send(outgoing);
		}
	}
}
