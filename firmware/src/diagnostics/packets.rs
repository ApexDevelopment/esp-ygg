//! Receives management-frame metadata without logging from the radio callback.

use core::cell::Cell;
use core::sync::atomic::{AtomicUsize, Ordering};
use embassy_sync::{
	blocking_mutex::{CriticalSectionMutex, raw::CriticalSectionRawMutex},
	channel::Channel,
};
use esp_radio::wifi::sniffer::PromiscuousPkt;

static STATION_MAC: CriticalSectionMutex<Cell<[u8; 6]>> =
	CriticalSectionMutex::new(Cell::new([0; 6]));
static RECEIVED: Channel<CriticalSectionRawMutex, Frame, 32> = Channel::new();
pub static DROPPED: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Frame {
	subtype: u8,
	source: [u8; 6],
	destination: [u8; 6],
	rssi: i32,
	channel: u32,
	code: Option<u16>,
}

pub fn set_station(mac: [u8; 6]) {
	STATION_MAC.lock(|station| station.set(mac));
}

pub fn receive(packet: PromiscuousPkt<'_>) {
	let data = packet.data;
	if data.len() < 24 || data[0] & 0x0c != 0 {
		return;
	}
	let subtype = data[0] >> 4;
	if !matches!(subtype, 1 | 3 | 10 | 11 | 12) {
		return;
	}
	let station = STATION_MAC.lock(|station| station.get());
	let mut source = [0; 6];
	let mut destination = [0; 6];
	source.copy_from_slice(&data[10..16]);
	destination.copy_from_slice(&data[4..10]);
	if source != station && destination != station {
		return;
	}
	let offset = match subtype {
		1 | 3 => 26,
		11 => 28,
		_ => 24,
	};
	let code = if data[1] & 0x40 == 0 {
		data.get(offset..offset + 2)
			.map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
	} else {
		None
	};
	let frame = Frame {
		subtype,
		source,
		destination,
		rssi: packet.rx_cntl.rssi,
		channel: packet.rx_cntl.channel,
		code,
	};
	if RECEIVED.try_send(frame).is_err() {
		DROPPED.fetch_add(1, Ordering::Relaxed);
	}
}

#[embassy_executor::task]
pub async fn log_frames() {
	loop {
		let frame = RECEIVED.receive().await;
		log::info!(
			"diag RX management: subtype={}, source={:02x?}, destination={:02x?}, RSSI={}, channel={}, status/reason={:?}",
			frame.subtype,
			frame.source,
			frame.destination,
			frame.rssi,
			frame.channel,
			frame.code
		);
	}
}
