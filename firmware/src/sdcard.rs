//! The microSD slot, which settings are exported to and imported from. The
//! board only touches its own folder on the card.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use embedded_hal::spi::{ErrorType, SpiBus};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use embedded_sdmmc::embedded_sdmmc_types::sdcard::CardType;
use embedded_sdmmc::{
	Error, Mode, SdCard, SdCardError, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Config, ConfigError, Spi};
use esp_hal::time::Rate;
use esp_ygg_app::time::DateTime;
use esp_ygg_app::ui::SETTINGS_FILE;
use log::warn;

/// Cards wake up at no more than 400 kHz, then all manage 20 MHz.
const WAKING_SPEED: Rate = Rate::from_khz(400);
const FULL_SPEED: Rate = Rate::from_mhz(20);
/// Settings files are far smaller; anything larger is not one.
const MAX_FILE_LEN: usize = 4096;

/// An SPI bus the card can be on.
pub trait SpiPort: SpiBus<u8> + ErrorType<Error = esp_hal::spi::Error> {
	fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError>;
}

impl SpiPort for Spi<'static, Blocking> {
	fn apply_config(&mut self, config: &Config) -> Result<(), ConfigError> {
		Spi::apply_config(self, config)
	}
}

pub struct Slot {
	bus: &'static RefCell<dyn SpiPort>,
	cs: Output<'static>,
	/// How to set the bus back up for whatever shares it, if anything does.
	shared_with: Option<Config>,
	/// The card, once woken, which stays awake while it has power. Waking
	/// it takes over half a second.
	woken: Option<CardType>,
}

type Card<'a> =
	SdCard<ExclusiveDevice<&'a mut dyn SpiPort, &'a mut Output<'static>, NoDelay>, Delay>;

impl Slot {
	/// The card's chip select must start high. On a shared bus, the others'
	/// must be high whenever they are not in use.
	pub fn new(
		bus: &'static RefCell<dyn SpiPort>,
		cs: Output<'static>,
		shared_with: Option<Config>,
	) -> Self {
		Slot {
			bus,
			cs,
			shared_with,
			woken: None,
		}
	}

	pub fn export_settings(&mut self, text: &str, time: Option<DateTime>) -> Result<(), String> {
		let (folder, file) = settings_path();
		self.with_card(|card| {
			let files = VolumeManager::new(card, Clock(time));
			let written = (|| {
				let volume = files.open_volume(VolumeIdx(0))?;
				let root = volume.open_root_dir()?;
				if let Err(Error::NotFound) = root.open_dir(folder).map(drop) {
					root.make_dir_in_dir(folder)?;
				}
				let folder = root.open_dir(folder)?;
				let file = folder.open_file_in_dir(file, Mode::ReadWriteCreateOrTruncate)?;
				file.write(text.as_bytes())?;
				file.flush()
			})();
			written.map_err(problem)
		})
	}

	pub fn import_settings(&mut self) -> Result<String, String> {
		let (folder, file) = settings_path();
		self.with_card(|card| {
			let files = VolumeManager::new(card, Clock(None));
			let read = (|| {
				let volume = files.open_volume(VolumeIdx(0))?;
				let root = volume.open_root_dir()?;
				let folder = root.open_dir(folder)?;
				let file = folder.open_file_in_dir(file, Mode::ReadOnly)?;
				let mut contents = Vec::new();
				let mut buffer = [0u8; 512];
				while !file.is_eof() && contents.len() <= MAX_FILE_LEN {
					let count = file.read(&mut buffer)?;
					contents.extend_from_slice(&buffer[..count]);
				}
				Ok(contents)
			})();
			match read {
				Ok(contents) if contents.len() > MAX_FILE_LEN => {
					Err("Settings file too big".into())
				}
				Ok(contents) => Ok(String::from_utf8_lossy(&contents).into_owned()),
				Err(Error::NotFound) => Err("No settings file".into()),
				Err(error) => Err(problem(error)),
			}
		})
	}

	/// Carries out a request of Share's, in the shared folder.
	#[cfg(feature = "share")]
	pub fn share(&mut self, request: crate::share::CardRequest) -> crate::share::CardReply {
		use crate::share::{CardReply, CardRequest};
		use esp_ygg_app::share::StoreError;
		// What to answer if there is no card to ask.
		let failed = Err(StoreError::Failed);
		let no_card = match &request {
			CardRequest::List => CardReply::List(failed.map(|()| Vec::new())),
			CardRequest::Size(_) => CardReply::Size(failed.map(|()| 0)),
			CardRequest::Read { .. } => CardReply::Read(failed.map(|()| Vec::new())),
			CardRequest::Create { .. } => CardReply::Create(failed.map(|()| String::new())),
			CardRequest::Append { .. } => CardReply::Done(failed),
		};
		let result = self.with_card(|card| {
			let files = VolumeManager::new(card, Clock(None));
			let volume = files.open_volume(VolumeIdx(0)).map_err(card_problem)?;
			let root = volume.open_root_dir().map_err(card_problem)?;
			let (outer, inner) = SHARE_FOLDER
				.split_once('/')
				.expect("the shared folder is in ours");
			if let Err(Error::NotFound) = root.open_dir(outer).map(drop) {
				root.make_dir_in_dir(outer).map_err(card_problem)?;
			}
			let ours = root.open_dir(outer).map_err(card_problem)?;
			if let Err(Error::NotFound) = ours.open_dir(inner).map(drop) {
				ours.make_dir_in_dir(inner).map_err(card_problem)?;
			}
			let folder = ours.open_dir(inner).map_err(card_problem)?;
			let open = |name: &str, mode: Mode| {
				folder
					.open_long_name_file_in_dir(name, mode)
					.or_else(|_| folder.open_file_in_dir(name, mode))
			};
			Ok(match request {
				CardRequest::List => {
					let mut storage = [0u8; 256];
					let mut names = embedded_sdmmc::LfnBuffer::new(&mut storage);
					let mut entries = Vec::new();
					let listed = folder.iterate_dir_lfn(&mut names, |entry, long| {
						if !entry.attributes.is_directory() && !entry.attributes.is_volume() {
							entries.push(esp_ygg_app::share::Entry {
								name: long
									.map_or_else(|| alloc::format!("{}", entry.name), String::from),
								size: entry.size.into(),
							});
						}
						core::ops::ControlFlow::Continue(())
					});
					CardReply::List(listed.map(|()| entries).map_err(store_error))
				}
				CardRequest::Size(name) => CardReply::Size(
					open(&name, Mode::ReadOnly)
						.map(|file| file.length().into())
						.map_err(store_error),
				),
				CardRequest::Read {
					name,
					offset,
					length,
				} => CardReply::Read(
					(|| {
						let file = open(&name, Mode::ReadOnly)?;
						file.seek_from_start(offset as u32)?;
						let mut data = alloc::vec![0u8; length];
						let mut filled = 0;
						while filled < length && !file.is_eof() {
							filled += file.read(&mut data[filled..])?;
						}
						data.truncate(filled);
						Ok(data)
					})()
					.map_err(store_error),
				),
				CardRequest::Create {
					name,
					keep_existing,
				} => CardReply::Create(
					(|| {
						// The name it is asked for if the card can hold it, or
						// a shortened one no other file has. One being read is
						// kept, rather than emptied as it goes.
						let mut number = u32::from(keep_existing);
						let short = loop {
							let short = esp_ygg_app::share::short_name(&name, number);
							let plain = number == 0 && short.eq_ignore_ascii_case(&name);
							if plain
								|| folder
									.open_file_in_dir(short.as_str(), Mode::ReadOnly)
									.is_err()
							{
								break short;
							}
							number += 1;
						};
						folder.open_file_in_dir(short.as_str(), Mode::ReadWriteCreateOrTruncate)?;
						Ok(short)
					})()
					.map_err(store_error),
				),
				CardRequest::Append { name, offset, data } => CardReply::Done(
					(|| {
						let file = folder.open_file_in_dir(name.as_str(), Mode::ReadWriteAppend)?;
						if file.offset() != offset as u32 {
							file.seek_from_start(offset as u32)?;
						}
						file.write(&data)?;
						file.flush()
					})()
					.map_err(store_error),
				),
			})
		});
		result.unwrap_or(no_card)
	}

	/// Runs `use_card` with the card, if there is one, woken and sped up,
	/// then hands the bus back as it was.
	fn with_card<R>(
		&mut self,
		use_card: impl FnOnce(Card<'_>) -> Result<R, String>,
	) -> Result<R, String> {
		let mut bus = self.bus.borrow_mut();
		let result = wake(&mut *bus, &mut self.cs, self.woken).and_then(|(card, card_type)| {
			self.woken = Some(card_type);
			use_card(card)
		});
		// The card may have been swapped, so is woken afresh next time.
		if result.is_err() {
			self.woken = None;
		}
		if let Some(config) = &self.shared_with
			&& let Err(error) = bus.apply_config(config)
		{
			warn!("Setting the shared SPI bus back up failed: {error:?}");
		}
		result
	}
}

/// Wakes the card, if there is one, and speeds it up. A card `woken` before
/// is taken as it is.
fn wake<'a>(
	bus: &'a mut dyn SpiPort,
	cs: &'a mut Output<'static>,
	woken: Option<CardType>,
) -> Result<(Card<'a>, CardType), String> {
	if let Some(card_type) = woken {
		let _ = bus.apply_config(&Config::default().with_frequency(FULL_SPEED));
		let device = ExclusiveDevice::new_no_delay(bus, cs)
			.map_err(|_| String::from("SD card chip select failed"))?;
		let card = SdCard::new(device, Delay::new());
		// SAFETY: the card was woken as this type. Were it swapped since,
		// the new one would not answer, and is woken afresh next time.
		unsafe { card.mark_card_as_init(card_type) };
		return Ok((card, card_type));
	}
	let _ = bus.apply_config(&Config::default().with_frequency(WAKING_SPEED));
	// Cards need 74 clocks with chip select high to wake into SPI mode.
	cs.set_high();
	let _ = bus.write(&[0xFF; 10]);
	let device = ExclusiveDevice::new_no_delay(bus, cs)
		.map_err(|_| String::from("SD card chip select failed"))?;
	let card = SdCard::new(device, Delay::new());
	if let Err(error) = card.num_bytes() {
		warn!("The SD card did not answer: {error:?}");
		return Err("No SD card".into());
	}
	card.spi(|device| {
		let _ = device
			.bus_mut()
			.apply_config(&Config::default().with_frequency(FULL_SPEED));
	});
	let card_type = card.get_card_type().ok_or("No SD card")?;
	Ok((card, card_type))
}

/// Where Share keeps the files it shares, inside the board's own folder.
#[cfg(feature = "share")]
const SHARE_FOLDER: &str = "esp-ygg/share";

#[cfg(feature = "share")]
fn card_problem(error: Error<SdCardError>) -> String {
	problem(error)
}

#[cfg(feature = "share")]
fn store_error(error: Error<SdCardError>) -> esp_ygg_app::share::StoreError {
	use esp_ygg_app::share::StoreError;
	match error {
		Error::NotFound => StoreError::NotFound,
		Error::NotEnoughSpace => StoreError::Full,
		Error::FilenameError(_) => StoreError::BadName,
		_ => StoreError::Failed,
	}
}

fn settings_path() -> (&'static str, &'static str) {
	SETTINGS_FILE
		.split_once('/')
		.expect("the settings file is in a folder")
}

fn problem(error: Error<SdCardError>) -> String {
	warn!("The SD card failed: {error}");
	match error {
		Error::DeviceError(_) => "SD card stopped answering".into(),
		Error::FormatError(_) | Error::NoSuchVolume => "SD card isn't FAT32".into(),
		Error::NotEnoughSpace => "SD card full".into(),
		_ => "SD card failed".into(),
	}
}

/// Dates files with the local time, once the board knows it.
struct Clock(Option<DateTime>);

impl TimeSource for Clock {
	fn get_timestamp(&self) -> Timestamp {
		self.0
			.and_then(|time| {
				let year = u16::try_from(time.year).ok()?;
				Timestamp::from_calendar(
					year,
					time.month,
					time.day,
					time.hour,
					time.minute,
					time.second,
				)
				.ok()
			})
			// The earliest date FAT stores
			.unwrap_or(Timestamp {
				year_since_1970: 10,
				zero_indexed_month: 0,
				zero_indexed_day: 0,
				hours: 0,
				minutes: 0,
				seconds: 0,
			})
	}
}
