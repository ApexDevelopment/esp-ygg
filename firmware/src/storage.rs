//! Stores the identity, clock floor, and settings in the shared NVS partition.
//! The clock starts ahead of its last save so message numbers do not go backwards.

use alloc::vec::Vec;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
	self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::rng::Rng;
use esp_nvs::{Key, Nvs, error::Error, platform::Crc};
use esp_storage::FlashStorage;
use esp_ygg_app::Settings;
use log::warn;

pub const SAVE_INTERVAL_SECS: u64 = 60 * 60;
const BUILD_TIME: &str = env!("ESP_YGG_BUILD_TIME");
const NAMESPACE: Key = Key::from_str("esp-ygg");
const IDENTITY: Key = Key::from_str("identity");
const SETTINGS: Key = Key::from_str("settings");
const CLOCK: Key = Key::from_str("clock");

struct NvsFlash(FlashStorage<'static>);
impl Crc for NvsFlash {
	fn crc32(init: u32, data: &[u8]) -> u32 {
		esp_hal::rom::crc::crc32_le(init, data)
	}
}
impl ErrorType for NvsFlash {
	type Error = <FlashStorage<'static> as ErrorType>::Error;
}
impl ReadNorFlash for NvsFlash {
	const READ_SIZE: usize = <FlashStorage<'static> as ReadNorFlash>::READ_SIZE;
	fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
		ReadNorFlash::read(&mut self.0, offset, bytes)
	}
	fn capacity(&self) -> usize {
		ReadNorFlash::capacity(&self.0)
	}
}
impl NorFlash for NvsFlash {
	const WRITE_SIZE: usize = <FlashStorage<'static> as NorFlash>::WRITE_SIZE;
	const ERASE_SIZE: usize = <FlashStorage<'static> as NorFlash>::ERASE_SIZE;
	fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
		NorFlash::write(&mut self.0, offset, bytes)
	}
	fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
		NorFlash::erase(&mut self.0, from, to)
	}
}

pub struct Flash {
	nvs: Nvs<NvsFlash>,
}
impl Flash {
	pub fn new(mut flash: FlashStorage<'static>) -> Self {
		#[cfg(feature = "diagnostics")]
		log::info!("diag flash: capacity={} bytes", flash.capacity());
		let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
		let table =
			partitions::read_partition_table(&mut flash, &mut buffer).unwrap_or_else(|error| {
				let mut probe = [0u8; 4];
				let read_error = flash.read(0x8000, &mut probe).err();
				panic!(
					"reading the partition table: {error:?}; flash capacity: {}; probe: {read_error:?}",
					flash.capacity()
				);
			});
		#[cfg(feature = "diagnostics")]
		for entry in table.iter() {
			log::info!(
				"diag partition: {:?}, offset={:#x}, length={:#x}",
				entry.label_as_str(),
				entry.offset(),
				entry.len()
			);
		}
		let partition = table
			.iter()
			.find(|entry| {
				entry.label_as_str() == "nvs"
					&& entry.partition_type() == PartitionType::Data(DataPartitionSubType::Nvs)
			})
			.expect("the partition table has no nvs partition");
		let nvs = Nvs::new(
			partition.offset() as usize,
			partition.len() as usize,
			NvsFlash(flash),
		)
		.expect("opening the nvs partition");
		Self { nvs }
	}
	pub fn settings(&mut self) -> Option<Settings> {
		let bytes: Vec<u8> = self.read(&SETTINGS)?;
		Settings::decode(&bytes)
	}
	pub fn open(mut self, rng: &Rng) -> (Store, u64) {
		let seed = match self.read::<Vec<u8>>(&IDENTITY) {
			Some(bytes) => bytes.try_into().expect("invalid saved identity length"),
			None => {
				warn!("No saved identity, generating a new private key");
				let mut seed = [0u8; 32];
				rng.read(&mut seed);
				self.nvs
					.set(&NAMESPACE, &IDENTITY, seed.as_slice())
					.expect("saving the identity");
				seed
			}
		};
		let build_time = BUILD_TIME.parse().unwrap_or(0);
		let saved = self.read::<u64>(&CLOCK).unwrap_or(build_time);
		let unix_base = saved.saturating_add(SAVE_INTERVAL_SECS).max(build_time);
		let mut store = Store { flash: self, seed };
		store.save_time(unix_base);
		(store, unix_base)
	}
	fn read<T>(&mut self, key: &Key) -> Option<T>
	where
		Nvs<NvsFlash>: esp_nvs::Get<T>,
	{
		match self.nvs.get(&NAMESPACE, key) {
			Ok(value) => Some(value),
			Err(Error::NamespaceNotFound | Error::KeyNotFound) => None,
			Err(error) => panic!("reading NVS key {key}: {error:?}"),
		}
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
	#[cfg_attr(not(feature = "ui"), allow(dead_code))]
	pub fn save_settings(&mut self, settings: &Settings) {
		let encoded = settings.encode();
		if let Some(Err(error)) = crate::park::while_parked(|| {
			self.flash
				.nvs
				.set(&NAMESPACE, &SETTINGS, encoded.as_slice())
		}) {
			warn!("Saving settings failed: {error:?}");
		}
	}
	pub fn save_time(&mut self, unix_time: u64) {
		if let Some(Err(error)) =
			crate::park::while_parked(|| self.flash.nvs.set(&NAMESPACE, &CLOCK, unix_time))
		{
			warn!("Saving the clock failed: {error:?}");
		}
	}
}
