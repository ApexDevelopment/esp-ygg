//! The microSD slot, which settings are exported to and imported from. The
//! board only touches its own folder on the card.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use embedded_sdmmc::{
	Error, Mode, SdCard, SdCardError, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_ygg_app::time::DateTime;
use esp_ygg_app::ui::SETTINGS_FILE;
use log::warn;

/// Cards wake up at no more than 400 kHz, then all manage 20 MHz.
const WAKING_SPEED: Rate = Rate::from_khz(400);
const FULL_SPEED: Rate = Rate::from_mhz(20);
/// Settings files are far smaller; anything larger is not one.
const MAX_FILE_LEN: usize = 4096;

pub type Bus = RefCell<Spi<'static, Blocking>>;

pub struct Slot {
	bus: &'static Bus,
	cs: Output<'static>,
	/// How to set the bus back up for whatever shares it, if anything does.
	shared_with: Option<Config>,
}

type Card<'a> = SdCard<
	ExclusiveDevice<&'a mut Spi<'static, Blocking>, &'a mut Output<'static>, NoDelay>,
	Delay,
>;

impl Slot {
	/// The card's chip select must start high. On a shared bus, the others'
	/// must be high whenever they are not in use.
	pub fn new(bus: &'static Bus, cs: Output<'static>, shared_with: Option<Config>) -> Self {
		Slot {
			bus,
			cs,
			shared_with,
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
					Err("The settings file is too big".into())
				}
				Ok(contents) => Ok(String::from_utf8_lossy(&contents).into_owned()),
				Err(Error::NotFound) => Err(format!("No {SETTINGS_FILE} found")),
				Err(error) => Err(problem(error)),
			}
		})
	}

	/// Runs `use_card` with the card, if there is one, woken and sped up,
	/// then hands the bus back as it was.
	fn with_card<R>(
		&mut self,
		use_card: impl FnOnce(Card<'_>) -> Result<R, String>,
	) -> Result<R, String> {
		let mut bus = self.bus.borrow_mut();
		let result = wake(&mut bus, &mut self.cs).and_then(use_card);
		if let Some(config) = &self.shared_with
			&& let Err(error) = bus.apply_config(config)
		{
			warn!("Setting the shared SPI bus back up failed: {error:?}");
		}
		result
	}
}

/// Wakes the card, if there is one, and speeds it up.
fn wake<'a>(
	bus: &'a mut Spi<'static, Blocking>,
	cs: &'a mut Output<'static>,
) -> Result<Card<'a>, String> {
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
	Ok(card)
}

fn settings_path() -> (&'static str, &'static str) {
	SETTINGS_FILE
		.split_once('/')
		.expect("the settings file is in a folder")
}

fn problem(error: Error<SdCardError>) -> String {
	warn!("The SD card failed: {error}");
	match error {
		Error::DeviceError(_) => "The SD card stopped answering".into(),
		Error::FormatError(_) | Error::NoSuchVolume => "Use an SD card formatted FAT32".into(),
		Error::NotEnoughSpace => "The SD card is full".into(),
		_ => "Reading the SD card failed".into(),
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
