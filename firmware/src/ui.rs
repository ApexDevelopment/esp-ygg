//! Runs the on-device UI and talks to the network side through channels.
//! On the Cardputer, the UI has the second core to itself, so drawing and
//! scanning keys never delay the radio and network work on the first. On
//! single-core boards, it runs in a thread below the network's priority, so
//! the network takes the core whenever it has work.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embedded_io_async::Read;
use esp_hal::peripherals::USB_DEVICE;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_ygg_app::ui::remote::{self, Remote};
use esp_ygg_app::ui::{self, App, Command, Key, Status, WifiNetwork};
use esp_ygg_app::{Event, Settings};
use log::warn;
use mousefood::EmbeddedBackend;
use mousefood::embedded_graphics::pixelcolor::Rgb565;
use ratatui::Terminal;
use yggdrasil_espnow::PublicKey;

#[cfg(feature = "cardputer")]
pub(crate) use crate::cardputer as board;
use crate::platform::now;
use crate::sdcard::Slot;
#[cfg(feature = "c6-touch-lcd")]
pub(crate) use crate::touch_lcd as board;

pub type Screen = Terminal<EmbeddedBackend<'static, board::Display, Rgb565>>;

/// How often the UI runs its timers when nothing else happens.
const TICK: Duration = Duration::from_millis(100);
/// The longest command line read from the serial port.
const MAX_COMMAND: usize = 512;

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
/// Key presses and taps, from the board's keyboard or touch screen, or the
/// serial port.
pub static INPUT: Channel<CriticalSectionRawMutex, Input, 16> = Channel::new();

pub enum Input {
	Key(Key),
	/// A tap on the touch screen, at a character cell.
	Tap {
		column: u16,
		row: u16,
	},
	/// A finger has touched the screen, at a character cell.
	Press {
		column: u16,
		row: u16,
	},
	Release,
}

/// Whether the serial port asked to see the screen.
static SCREEN_WANTED: AtomicBool = AtomicBool::new(false);

/// Starts the UI task and the serial port's, once the board has its screen.
pub fn spawn(
	spawner: &Spawner,
	screen: Screen,
	slot: Slot,
	usb: USB_DEVICE<'static>,
	settings: Settings,
) {
	spawner.spawn(run(screen, slot, settings).expect("spawning the UI task"));
	spawner.spawn(serial(usb).expect("spawning the serial task"));
}

#[embassy_executor::task]
async fn run(mut screen: Screen, mut slot: Slot, settings: Settings) {
	let mut app = App::new(settings, now());
	loop {
		#[cfg(feature = "cardputer")]
		second_core::report_stack();
		match select3(INPUT.receive(), TO_UI.receive(), Timer::after(TICK)).await {
			Either3::First(Input::Key(key)) => app.handle_key(key),
			Either3::First(Input::Tap { column, row }) => app.handle_tap(column, row),
			Either3::First(Input::Press { column, row }) => app.handle_press(column, row),
			Either3::First(Input::Release) => app.handle_release(),
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
				// The card is on this side, and quick enough to wait for.
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
						INPUT.send(Input::Key(key)).await;
					}
				}
				Ok(Remote::Tap(column, row)) => {
					INPUT.send(Input::Press { column, row }).await;
					INPUT.send(Input::Tap { column, row }).await;
					INPUT.send(Input::Release).await;
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

/// Starting the UI on the second core, with a stack of its own.
#[cfg(feature = "cardputer")]
pub mod second_core {
	use core::sync::atomic::{AtomicUsize, Ordering};

	#[cfg(feature = "voice")]
	use esp_hal::peripherals::FROM_CPU_INTR2;
	use esp_hal::peripherals::{CPU_CTRL, FROM_CPU_INTR1, FROM_CPU_INTR3, USB_DEVICE};
	use esp_hal::system::Stack;
	use esp_rtos::embassy::Executor;
	use esp_ygg_app::Settings;
	use log::info;
	use static_cell::StaticCell;

	#[cfg(feature = "voice")]
	use crate::cardputer::Model;
	use crate::cardputer::{self, Pins};

	/// TLS for Gemini goes deepest, at 14 KB in a handshake with an elliptic
	/// curve certificate. RSA certificates need more.
	const STACK_SIZE: usize = 24 * 1024;
	/// Fills the UI's stack at first, to tell later how deep it has been used.
	const UNUSED: u8 = 0xa5;
	static STACK_BOTTOM: AtomicUsize = AtomicUsize::new(0);
	static STACK_REPORTED: AtomicUsize = AtomicUsize::new(0);

	/// Starts the screen, the keyboard and the UI on the second core.
	pub fn start(
		cpu_control: CPU_CTRL<'static>,
		(interrupt, park): (FROM_CPU_INTR1<'static>, FROM_CPU_INTR3<'static>),
		pins: Pins,
		sd: cardputer::SdPins,
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
				let (_model, screen) = cardputer::start(&spawner, pins);
				#[cfg(feature = "voice")]
				start_audio(audio, _model);
				let slot = cardputer::sd_slot(sd);
				super::spawn(&spawner, screen, slot, usb, settings);
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

	/// Logs each further kilobyte of the UI's stack that has been used.
	pub fn report_stack() {
		let used = stack_used();
		if used >= STACK_REPORTED.load(Ordering::Relaxed) + 1024 {
			info!(
				"The UI has used {} of {} KB of its stack",
				used / 1024,
				STACK_SIZE / 1024
			);
			STACK_REPORTED.store(used, Ordering::Relaxed);
		}
	}

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
}

/// Starting the UI in a thread of its own on single-core boards, below the
/// network's priority, so drawing never holds up the radio.
#[cfg(feature = "c6-touch-lcd")]
pub mod own_thread {
	use alloc::boxed::Box;
	use core::ffi::c_void;

	use esp_hal::peripherals::USB_DEVICE;
	use esp_rtos::CurrentThreadHandle;
	use esp_rtos::embassy::Executor;
	use esp_ygg_app::Settings;
	use static_cell::StaticCell;

	use crate::touch_lcd::{self, Pins};

	/// TLS for Gemini goes deepest, at 14 KB in a handshake with an elliptic
	/// curve certificate. RSA certificates need more.
	const STACK_SIZE: usize = 24 * 1024;
	/// WiFi's own threads run above both of these.
	const NETWORK_PRIORITY: usize = 1;
	const UI_PRIORITY: u32 = 0;

	struct Start {
		pins: Pins,
		usb: USB_DEVICE<'static>,
		settings: Settings,
	}

	/// Raises the calling thread, which runs the network, above the UI, and
	/// starts the screen and the UI in a thread of their own.
	pub fn start(pins: Pins, usb: USB_DEVICE<'static>, settings: Settings) {
		CurrentThreadHandle::get().set_priority(NETWORK_PRIORITY);
		let start = Box::into_raw(Box::new(Start {
			pins,
			usb,
			settings,
		}));
		// SAFETY: the thread takes ownership of `start`, which nothing else
		// holds on to.
		unsafe {
			esp_radio_rtos_driver::task_create(
				"ui",
				run,
				start.cast(),
				UI_PRIORITY,
				None,
				STACK_SIZE,
			);
		}
	}

	extern "C" fn run(start: *mut c_void) {
		// SAFETY: `start` came from `Box::into_raw` in `start`, and is only
		// taken back here.
		let start = unsafe { Box::from_raw(start.cast::<Start>()) };
		static EXECUTOR: StaticCell<Executor> = StaticCell::new();
		EXECUTOR.init(Executor::new()).run(move |spawner| {
			let Start {
				pins,
				usb,
				settings,
			} = *start;
			let (screen, slot) = touch_lcd::start(&spawner, pins);
			super::spawn(&spawner, screen, slot, usb, settings);
		});
	}
}
