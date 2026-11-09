//! Keeps the node's private key, a floor for its clock, and the user's
//! settings in the flash `nvs` partition.
//!
//! Yggdrasil numbers some messages with Unix time, and other nodes ignore
//! numbers that go backwards. Without a real-time clock, the node starts each
//! boot from the last time it saved plus [`SAVE_INTERVAL`], and saves at least
//! that often, so its clock never goes backwards across reboots. It runs ahead
//! of real time instead, which does no harm.

use alloc::vec;
use esp_bootloader_esp_idf::partitions::{
	self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::rng::Rng;
use esp_storage::FlashStorage;
use esp_ygg_app::Settings;
use log::warn;

pub const SAVE_INTERVAL_SECS: u64 = 60 * 60;

const MAGIC: [u8; 4] = *b"YGG1";
const RECORD_LEN: usize = 4 + 32 + 8;
const BUILD_TIME: &str = env!("ESP_YGG_BUILD_TIME");
/// Settings live in the next flash sector, so saving the clock every hour
/// never rewrites them.
const SETTINGS_OFFSET: u32 = 0x1000;
const MAX_SETTINGS_LEN: usize = 1024;

/// The flash partition, before the identity is loaded.
pub struct Flash {
	flash: FlashStorage<'static>,
	offset: u32,
}

impl Flash {
	pub fn new(mut flash: FlashStorage<'static>) -> Self {
		let offset = nvs_offset(&mut flash);
		Flash { flash, offset }
	}

	/// The saved settings, if there are any.
	pub fn settings(&mut self) -> Option<Settings> {
		let mut length = [0u8; 2];
		self.flash
			.read(self.offset + SETTINGS_OFFSET, &mut length)
			.ok()?;
		let length = usize::from(u16::from_le_bytes(length));
		if length > MAX_SETTINGS_LEN {
			return None;
		}
		let mut bytes = vec![0u8; length];
		self.flash
			.read(self.offset + SETTINGS_OFFSET + 2, &mut bytes)
			.ok()?;
		Settings::decode(&bytes)
	}

	/// Loads the saved identity, or creates one. Returns the store and the
	/// Unix time to start this boot's clock from.
	pub fn open(mut self, rng: &Rng) -> (Store, u64) {
		let mut record = [0u8; RECORD_LEN];
		self.flash
			.read(self.offset, &mut record)
			.expect("reading identity from flash");
		let build_time: u64 = BUILD_TIME.parse().unwrap_or(0);
		let (seed, unix_base) = if record[..4] == MAGIC {
			let seed = record[4..36].try_into().expect("seed length");
			let saved = u64::from_le_bytes(record[36..].try_into().expect("time length"));
			(seed, (saved + SAVE_INTERVAL_SECS).max(build_time))
		} else {
			warn!("No saved identity, generating a new private key");
			let mut seed = [0u8; 32];
			rng.read(&mut seed);
			(seed, build_time)
		};
		let mut store = Store { flash: self, seed };
		store.save_time(unix_base);
		(store, unix_base)
	}
}

pub struct Store {
	flash: Flash,
	seed: [u8; 32],
}

impl Store {
	pub fn seed(&self) -> &[u8; 32] {
		&self.seed
	}

	// Only boards with a UI change settings.
	#[cfg_attr(not(feature = "cardputer"), allow(dead_code))]
	pub fn save_settings(&mut self, settings: &Settings) {
		let encoded = settings.encode();
		let mut record = (encoded.len() as u16).to_le_bytes().to_vec();
		record.extend_from_slice(&encoded);
		let offset = self.flash.offset + SETTINGS_OFFSET;
		if let Err(error) = self.flash.flash.write(offset, &record) {
			warn!("Saving settings failed: {error:?}");
		}
	}

	pub fn save_time(&mut self, unix_time: u64) {
		let mut record = [0u8; RECORD_LEN];
		record[..4].copy_from_slice(&MAGIC);
		record[4..36].copy_from_slice(&self.seed);
		record[36..].copy_from_slice(&unix_time.to_le_bytes());
		if let Err(error) = self.flash.flash.write(self.flash.offset, &record) {
			warn!("Saving to flash failed: {error:?}");
		}
	}
}

fn nvs_offset(flash: &mut FlashStorage<'static>) -> u32 {
	let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
	let table =
		partitions::read_partition_table(flash, &mut table).expect("reading the partition table");
	let nvs = table
		.find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
		.expect("reading the partition table")
		.expect("the partition table has no nvs partition");
	nvs.offset()
}
