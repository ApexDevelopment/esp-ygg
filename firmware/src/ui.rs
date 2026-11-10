//! Runs the on-device UI on the second core, so drawing and scanning keys
//! never delay the radio and network work on the first. The cores exchange
//! messages through channels.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::{CPU_CTRL, FROM_CPU_INTR1};
use esp_hal::system::Stack;
use esp_rtos::embassy::Executor;
use esp_ygg_app::ui::{self, App, Command, Status, WifiNetwork};
use esp_ygg_app::{Event, Settings};
use log::info;
use static_cell::StaticCell;
use yggdrasil_espnow::PublicKey;

use crate::cardputer::{self, KEYS, Model, Pins, Screen};
use crate::platform::now;

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
	interrupt: FROM_CPU_INTR1<'static>,
	pins: Pins,
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
	esp_rtos::start_second_core(
		cpu_control,
		interrupt,
		stack,
		move || {
			static EXECUTOR: StaticCell<Executor> = StaticCell::new();
			EXECUTOR.init(Executor::new()).run(|spawner| {
				let (model, screen) = cardputer::start(&spawner, pins);
				MODEL.store(model as u8 + 1, Ordering::Relaxed);
				spawner.spawn(run(screen, settings).expect("spawning the UI task"));
			});
		},
	);
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
async fn run(mut screen: Screen, settings: Settings) {
	let mut app = App::new(settings, now());
	let mut stack_reported = 0;
	loop {
		let used = stack_used();
		if used >= stack_reported + 1024 {
			info!("The UI has used {} of {} KB of its stack", used / 1024, STACK_SIZE / 1024);
			stack_reported = used;
		}
		match select3(KEYS.receive(), TO_UI.receive(), Timer::after(TICK)).await {
			Either3::First(key) => app.handle_key(key),
			Either3::Second(message) => handle_message(&mut app, message),
			Either3::Third(()) => {}
		}
		app.tick(now());
		while let Some(command) = app.poll_command() {
			FROM_UI.send(command).await;
		}
		let _ = screen.draw(|frame| ui::draw(frame, &app));
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
