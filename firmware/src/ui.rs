//! Runs the on-device UI on the second core, so drawing and scanning keys
//! never delay the radio and network work on the first. The cores exchange
//! messages through channels.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embedded_io_async::Read;
#[cfg(feature = "voice")]
use esp_hal::peripherals::FROM_CPU_INTR2;
use esp_hal::peripherals::{CPU_CTRL, FROM_CPU_INTR1, FROM_CPU_INTR3, USB_DEVICE};
use esp_hal::system::Stack;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_rtos::embassy::Executor;
use esp_ygg_app::ui::remote::{self, Remote};
use esp_ygg_app::ui::{self, App, Command, Status, WifiNetwork};
use esp_ygg_app::{Event, Settings};
use log::{info, warn};
use static_cell::StaticCell;
use yggdrasil_espnow::PublicKey;

use crate::cardputer::{self, KEYS, Model, Pins, Screen};
use crate::platform::now;
use crate::sdcard::Slot;

/// How often the UI runs its timers when nothing else happens.
const TICK: Duration = Duration::from_millis(100);
/// TLS for Gemini goes deepest, at 14 KB in a handshake with an elliptic
/// curve certificate. RSA certificates need more.
const STACK_SIZE: usize = 24 * 1024;

/// What the network side tells the UI.
pub enum ToUi {
	Status(Status),
	Event(Event),
	NodeInfo(PublicKey, Option<Vec<u8>>),
	Networks(Vec<WifiNetwork>),
}

pub static TO_UI: Channel<CriticalSectionRawMutex, ToUi, 16> = Channel::new();
/// What the UI asks the network side to do.
pub static FROM_UI: Channel<CriticalSectionRawMutex, Command, 8> = Channel::new();

/// 0 until the second core has detected the board.
static MODEL: AtomicU8 = AtomicU8::new(0);
/// Whether the serial port asked to see the screen.
static SCREEN_WANTED: AtomicBool = AtomicBool::new(false);
/// The longest command line read from the serial port.
const MAX_COMMAND: usize = 512;
/// Fills the UI's stack at first, to tell later how deep it has been used.
const UNUSED: u8 = 0xa5;
static STACK_BOTTOM: AtomicUsize = AtomicUsize::new(0);

/// The most of the UI's stack used so far. The stack grows down, so this
/// counts from the bottom up to the first byte that is no longer marked.
fn stack_used() -> usize {
	let bottom = STACK_BOTTOM.load(Ordering::Relaxed) as *const u8;
	// The bottom holds esp-rtos's guard, which is not marked.
	const GUARD: usize = 256;
	let unused = (GUARD..STACK_SIZE)
		// SAFETY: reads within the stack, which lives forever.
		.take_while(|&offset| unsafe { bottom.add(offset).read_volatile() } == UNUSED)
		.count();
	STACK_SIZE - GUARD - unused
}

/// Starts the screen, the keyboard and the UI on the second core.
pub fn start(
	cpu_control: CPU_CTRL<'static>,
	(interrupt, park): (FROM_CPU_INTR1<'static>, FROM_CPU_INTR3<'static>),
	pins: Pins,
	sd: crate::sdcard::Pins,
	usb: USB_DEVICE<'static>,
	#[cfg(feature = "voice")] audio: (crate::audio::Pins, FROM_CPU_INTR2<'static>),
	settings: Settings,
) {
	static STACK: StaticCell<Stack<STACK_SIZE>> = StaticCell::new();
	let stack = STACK.init(Stack::new());
	// SAFETY: the stack is not in use until the core starts below.
	unsafe {
		stack
			.mem
			.as_mut_ptr()
			.cast::<u8>()
			.write_bytes(UNUSED, STACK_SIZE);
	}
	STACK_BOTTOM.store(stack.mem.as_ptr() as usize, Ordering::Relaxed);
	esp_rtos::start_second_core(cpu_control, interrupt, stack, move || {
		static EXECUTOR: StaticCell<Executor> = StaticCell::new();
		crate::park::arm(park);
		EXECUTOR.init(Executor::new()).run(|spawner| {
			let (model, screen) = cardputer::start(&spawner, pins);
			MODEL.store(model as u8 + 1, Ordering::Relaxed);
			#[cfg(feature = "voice")]
			start_audio(audio, model);
			let slot = crate::sdcard::Slot::new(sd);
			spawner.spawn(run(screen, slot, settings).expect("spawning the UI task"));
			spawner.spawn(serial(usb).expect("spawning the serial task"));
		});
	});
}

/// Runs audio from an interrupt, so it goes ahead of drawing the screen
/// and the DMA never runs dry.
#[cfg(feature = "voice")]
fn start_audio((pins, interrupt): (crate::audio::Pins, FROM_CPU_INTR2<'static>), model: Model) {
	use esp_rtos::embassy::InterruptExecutor;
	static EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();
	let spawner = EXECUTOR
		.init(InterruptExecutor::new(interrupt))
		.start(esp_hal::interrupt::Priority::Priority1);
	spawner.spawn(crate::audio::run(pins, model).expect("spawning the audio task"));
}

/// The board's name, once the second core has detected it.
pub fn model_name() -> &'static str {
	match MODEL.load(Ordering::Relaxed) {
		1 => Model::Original.name(),
		2 => Model::Adv.name(),
		_ => "M5Stack Cardputer",
	}
}

#[embassy_executor::task]
async fn run(mut screen: Screen, mut slot: Slot, settings: Settings) {
	let mut app = App::new(settings, now());
	let mut stack_reported = 0;
	loop {
		let used = stack_used();
		if used >= stack_reported + 1024 {
			info!(
				"The UI has used {} of {} KB of its stack",
				used / 1024,
				STACK_SIZE / 1024
			);
			stack_reported = used;
		}
		match select3(KEYS.receive(), TO_UI.receive(), Timer::after(TICK)).await {
			Either3::First(key) => app.handle_key(key),
			Either3::Second(message) => handle_message(&mut app, message),
			Either3::Third(()) => {}
		}
		#[cfg(feature = "voice")]
		{
			app.set_talking(crate::audio::TALKING.load(Ordering::Relaxed));
			crate::speaker::show(app.speaker(crate::audio::MUTED.load(Ordering::Relaxed)));
		}
		app.tick(now());
		while let Some(command) = app.poll_command() {
			match command {
				// The card is on this core's side, and quick enough to wait for.
				Command::ExportSettings(text) => {
					app.settings_exported(slot.export_settings(&text, app.local_time()))
				}
				Command::ImportSettings => app.settings_imported(slot.import_settings()),
				command => FROM_UI.send(command).await,
			}
		}
		if let Ok(frame) = screen.draw(|frame| ui::draw(frame, &app))
			&& SCREEN_WANTED.swap(false, Ordering::Relaxed)
		{
			esp_println::println!("{}", remote::SCREEN_START);
			for line in remote::marked_screen_text(frame.buffer) {
				esp_println::println!("{line}");
			}
			esp_println::println!("{}", remote::SCREEN_END);
		}
	}
}

/// Takes commands from the USB serial port, as [`remote`] describes, so the
/// interface can be driven from a computer.
#[embassy_executor::task]
async fn serial(usb: USB_DEVICE<'static>) {
	let (mut receiver, _transmitter) = UsbSerialJtag::new(usb).into_async().split();
	let mut line = Vec::new();
	let mut buffer = [0u8; 64];
	loop {
		let Ok(count) = receiver.read(&mut buffer).await;
		for &byte in &buffer[..count] {
			if byte != b'\n' {
				if line.len() < MAX_COMMAND {
					line.push(byte);
				}
				continue;
			}
			let text = String::from_utf8_lossy(&line);
			match remote::parse(&text) {
				Ok(Remote::Keys(keys)) => {
					for key in keys {
						KEYS.send(key).await;
					}
				}
				Ok(Remote::Screen) => SCREEN_WANTED.store(true, Ordering::Relaxed),
				#[cfg(feature = "voice")]
				Ok(Remote::Talk(on)) => crate::audio::REMOTE_TALK.store(on, Ordering::Relaxed),
				#[cfg(feature = "voice")]
				Ok(Remote::Mute(on)) => crate::audio::MUTED.store(on, Ordering::Relaxed),
				#[cfg(not(feature = "voice"))]
				Ok(Remote::Talk(_) | Remote::Mute(_)) => warn!("Voice is not in this build"),
				Err(error) => warn!("{error}"),
			}
			line.clear();
		}
	}
}

fn handle_message(app: &mut App, message: ToUi) {
	match message {
		ToUi::Status(status) => app.set_status(status),
		ToUi::Event(event) => app.handle_event(event),
		ToUi::NodeInfo(key, info) => app.handle_nodeinfo(key, info),
		ToUi::Networks(networks) => app.set_networks(networks),
	}
}
