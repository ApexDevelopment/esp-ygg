#![no_std]
#![no_main]

extern crate alloc;

#[cfg(all(feature = "tui", feature = "gui"))]
compile_error!("Pick one interface, tui or gui, not both");
#[cfg(all(
	feature = "tui",
	not(any(feature = "cardputer", feature = "c6-touch-lcd"))
))]
compile_error!("The text interface needs a board with a screen: cardputer or c6-touch-lcd");
#[cfg(all(feature = "gui", not(feature = "c6-touch-lcd")))]
compile_error!("The graphical interface only runs on the touch board, c6-touch-lcd");

#[cfg(feature = "voice")]
mod audio;
#[cfg(feature = "voice")]
mod call;
#[cfg(feature = "cardputer")]
mod cardputer;
#[cfg(feature = "gui")]
mod gui;
mod hotspot;
#[cfg(feature = "ui")]
mod lock;
#[cfg(feature = "ntp")]
mod ntp;
mod park;
mod platform;
mod radio;
#[cfg(feature = "ui")]
mod sdcard;
#[cfg(all(feature = "sftp", feature = "ui"))]
mod sftp;
#[cfg(all(feature = "sftp", not(feature = "ui")))]
compile_error!("SFTP keeps its files on the SD card, so needs a board with an interface");
#[cfg(feature = "voice")]
mod speaker;
mod storage;
#[cfg(feature = "c6-touch-lcd")]
mod touch_lcd;
#[cfg(feature = "ui")]
mod ui;
mod uplink;

use alloc::vec::Vec;
use core::net::Ipv6Addr;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, Either4, select3, select4};
use embassy_net::StackResources;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{
	BROADCAST_ADDRESS, EspNowManager, EspNowReceiver, EspNowWifiInterface, PeerInfo, ReceivedData,
};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, ControllerConfig, Interface, WifiController};
use esp_storage::FlashStorage;
#[cfg(feature = "ui")]
use esp_ygg_app::ui::{Command, Status, Uplink, WifiNetwork};
use esp_ygg_app::{BoardInfo, Overlay, Settings};
use log::{info, warn};
use static_cell::StaticCell;
use yggdrasil_espnow::yggdrasil_proto::requests::{BuildInfo, build_nodeinfo};
use yggdrasil_espnow::{Config, Destination, FRAME_LEN, Mac, Node, PrivateKey, StreamId};

use platform::{EspPlatform, to_embassy};
use storage::{SAVE_INTERVAL_SECS, Store};
use uplink::{CHUNK, TO_NODE, TO_SOCKET, ToNode, ToSocket};

esp_bootloader_esp_idf::esp_app_desc!();

const DEFAULT_CHANNEL: u8 = 1;
/// ESP-NOW keeps a peer list of its own, with room for this many.
const MAX_ESPNOW_PEERS: usize = 20;
const STALL_WARNING: Duration = Duration::from_millis(500);
/// How often the UI gets a fresh status.
#[cfg(feature = "ui")]
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(feature = "esp32s3")]
const CHIP: &str = "esp32s3";
#[cfg(feature = "esp32c3")]
const CHIP: &str = "esp32c3";
#[cfg(feature = "esp32c6")]
const CHIP: &str = "esp32c6";
#[cfg(feature = "esp32")]
const CHIP: &str = "esp32";

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
	esp_println::logger::init_logger_from_env();
	#[allow(unused_mut)]
	let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
	#[cfg(feature = "esp32s3")]
	{
		use esp_hal::system::{Cpu, CpuControl};
		let mut control = CpuControl::new(peripherals.CPU_CTRL.reborrow());
		// A launcher can leave the other core enabled across a software reset.
		// SAFETY: startup runs on the first core, before we start the second.
		unsafe { control.park_core(Cpu::AppCpu) };
	}
	esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
	// The original ESP32 has less RAM to spare
	#[cfg(not(any(feature = "esp32", all(feature = "cardputer", feature = "tui"))))]
	esp_alloc::heap_allocator!(size: 200 * 1024);
	// The UI's core has a stack of its own, so leave room for it, and for
	// the larger one SFTP needs.
	#[cfg(all(feature = "cardputer", feature = "tui"))]
	esp_alloc::heap_allocator!(size: if cfg!(feature = "sftp") { 168 * 1024 } else { 176 * 1024 });
	#[cfg(feature = "esp32")]
	esp_alloc::heap_allocator!(size: 120 * 1024);
	let timg0 = TimerGroup::new(peripherals.TIMG0);
	esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

	// The random number generator is only truly random once the radio runs,
	// so the identity is read or created after WiFi starts.
	let flash = FlashStorage::new(peripherals.FLASH);
	// SAFETY: the other core only runs once the UI starts, and from then on
	// parks itself for each write, as `park` describes.
	#[cfg(any(feature = "esp32s3", feature = "esp32"))]
	let flash = unsafe { flash.multicore_ignore() };
	let mut settings_store = storage::Flash::new(flash);
	#[cfg_attr(not(feature = "ui"), allow(unused_mut))]
	let mut settings = settings_store
		.settings()
		.unwrap_or_else(build_time_settings);
	let station = settings
		.bridge
		.then(|| uplink::station_config(&settings.ssid, &settings.password))
		.flatten();
	let bridge = station.is_some() && settings.can_bridge();
	let station = station.filter(|_| bridge);
	let access_point = settings
		.hotspot
		.then(|| {
			hotspot::config(
				&settings.hotspot_name,
				&settings.hotspot_password,
				settings.channel,
			)
		})
		.flatten();
	let hotspot_on = access_point.is_some();
	let mut controller_config = ControllerConfig::default();
	match (station, access_point) {
		(Some(station), None) => {
			controller_config = controller_config.with_initial_config(WifiConfig::Station(station));
		}
		// ESP-NOW sends through the station interface, so it stays up even
		// when not bridging.
		(station, Some(access_point)) => {
			let station = station.unwrap_or_else(StationConfig::default);
			controller_config = controller_config
				.with_initial_config(WifiConfig::AccessPointStation(station, access_point));
		}
		(None, None) => {}
	}
	info!(
		"Starting WiFi: bridge={bridge}, hotspot={hotspot_on}, free heap={} bytes",
		esp_alloc::HEAP.free()
	);
	let controller =
		WifiController::new(peripherals.WIFI, controller_config).expect("starting WiFi");
	let rng = Rng::new();
	let (mut store, unix_base) = settings_store.open(&rng);
	#[cfg(feature = "sftp")]
	sftp::set_up(store.seed(), &settings.sftp_password);

	let esp_now = controller.esp_now();
	info!("ESP-NOW version {}", esp_now.version().unwrap_or(0));
	if !bridge {
		esp_now
			.set_channel(settings.channel)
			.expect("setting the WiFi channel");
		info!("Using WiFi channel {}", settings.channel);
	}
	let (manager, sender, mut receiver) = esp_now.split();
	spawner.spawn(radio::sender(sender).expect("spawning the radio task"));
	spawner.spawn(uplink::wifi(controller, bridge).expect("spawning the WiFi task"));
	if bridge {
		let peer = settings.peer.parse().expect("checked by can_bridge");
		let (stack, runner) = embassy_net::new(
			esp_radio::wifi::Interface::station(),
			embassy_net::Config::dhcpv4(Default::default()),
			{
				// DHCP, DNS, the TCP peering and NTP
				static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
				RESOURCES.init(StackResources::new())
			},
			u64::from(rng.random()) << 32 | u64::from(rng.random()),
		);
		spawner.spawn(uplink::network(runner).expect("spawning the network task"));
		spawner.spawn(uplink::tcp(stack, peer).expect("spawning the TCP task"));
		#[cfg(feature = "ntp")]
		spawner.spawn(ntp::ntp(stack).expect("spawning the NTP task"));
		info!("Peering with {peer} over WiFi network {}", settings.ssid);
	}

	let mut node = Node::new(
		node_config(&store, &settings),
		EspPlatform::new(rng, unix_base),
	)
	.expect("starting the Yggdrasil node");
	info!("Public key: {}", Hex(&node.public_key().0));
	info!("Address: {}", Ipv6Addr::from(node.address().0));
	info!("Subnet: {}/64", Ipv6Addr::from(subnet_prefix(&node)));
	let mut overlay = Overlay::new(
		&node,
		u64::from(rng.random()) << 32 | u64::from(rng.random()),
		settings.status_page,
	);
	#[cfg(feature = "voice")]
	{
		overlay.start_phone(phone_name(&settings), rng.random() as u16);
		audio::set_ringer(settings.ringtone, settings.ring_volume);
	}
	if hotspot_on {
		let access_point = Interface::access_point();
		overlay.start_hotspot(&node, access_point.mac_address());
		spawner.spawn(hotspot::link(access_point).expect("spawning the hotspot task"));
		info!("Hotspot {} is on", settings.hotspot_name);
	}

	#[cfg(all(feature = "cardputer", not(feature = "tui")))]
	cardputer::screen_off(peripherals.GPIO38);
	#[cfg(all(feature = "cardputer", feature = "tui"))]
	{
		let pins = cardputer::Pins {
			spi: peripherals.SPI2,
			sck: peripherals.GPIO36,
			mosi: peripherals.GPIO35,
			cs: peripherals.GPIO37,
			dc: peripherals.GPIO34,
			reset: peripherals.GPIO33,
			backlight: peripherals.GPIO38,
			i2c: peripherals.I2C0,
			gpio8: peripherals.GPIO8,
			gpio9: peripherals.GPIO9,
			gpio11: peripherals.GPIO11,
			gpio13: peripherals.GPIO13,
			gpio15: peripherals.GPIO15,
			gpio3: peripherals.GPIO3,
			gpio4: peripherals.GPIO4,
			gpio5: peripherals.GPIO5,
			gpio6: peripherals.GPIO6,
			gpio7: peripherals.GPIO7,
			#[cfg(not(feature = "voice"))]
			g0: peripherals.GPIO0,
		};
		ui::second_core::start(
			peripherals.CPU_CTRL,
			(peripherals.FROM_CPU_INTR1, peripherals.FROM_CPU_INTR3),
			pins,
			cardputer::SdPins {
				spi: peripherals.SPI3,
				sck: peripherals.GPIO40,
				miso: peripherals.GPIO39,
				mosi: peripherals.GPIO14,
				cs: peripherals.GPIO12,
			},
			peripherals.USB_DEVICE,
			#[cfg(feature = "voice")]
			(
				audio::Pins {
					i2s: peripherals.I2S0,
					dma: peripherals.DMA_CH0,
					button: peripherals.GPIO0,
					bclk: peripherals.GPIO41,
					dout: peripherals.GPIO42,
					ws: peripherals.GPIO43,
					din: peripherals.GPIO46,
				},
				peripherals.FROM_CPU_INTR2,
			),
			settings.clone(),
		);
	}
	#[cfg(all(feature = "c6-touch-lcd", not(feature = "ui")))]
	touch_lcd::screen_off(peripherals.I2C0, peripherals.GPIO6, peripherals.GPIO7);
	#[cfg(all(feature = "c6-touch-lcd", feature = "ui"))]
	{
		let pins = touch_lcd::Pins {
			spi: peripherals.SPI2,
			sck: peripherals.GPIO0,
			mosi: peripherals.GPIO1,
			cs: peripherals.GPIO11,
			dc: peripherals.GPIO10,
			sd_cs: peripherals.GPIO23,
			sd_miso: peripherals.GPIO8,
			i2c: peripherals.I2C0,
			sda: peripherals.GPIO6,
			scl: peripherals.GPIO7,
			boot_button: peripherals.GPIO9,
			dma: peripherals.DMA_CH0,
		};
		info!(
			"Starting UI thread, free heap={} bytes",
			esp_alloc::HEAP.free()
		);
		ui::own_thread::start(pins, peripherals.USB_DEVICE, settings.clone());
	}
	#[cfg(feature = "ui")]
	let mut next_status = Instant::now();
	#[cfg(feature = "voice")]
	let mut call = call::CallLink::default();

	info!("Node ready, free heap={} bytes", esp_alloc::HEAP.free());
	let mut frame = alloc::vec![0u8; FRAME_LEN];
	let mut stream: Option<StreamId> = None;
	let mut next_save = Instant::now() + Duration::from_secs(SAVE_INTERVAL_SECS);
	#[cfg(feature = "sftp")]
	let mut sftp_pump = sftp::Pump::default();
	loop {
		let started = Instant::now();
		send_frames(&mut node, &manager, &mut frame);
		let backlogged = flush_stream(&mut node, &mut stream) || radio::OUTBOX.is_full();
		#[cfg(feature = "ui")]
		let backlogged = backlogged || ui::TO_UI.is_full();

		let mut wake = [
			node.poll_timeout().map(to_embassy),
			overlay.poll_at(node.now()).map(to_embassy),
		]
		.into_iter()
		.flatten()
		.min()
		.unwrap_or_else(|| Instant::now() + Duration::from_secs(1))
		.min(next_save);
		#[cfg(feature = "ui")]
		{
			wake = wake.min(next_status);
		}
		if backlogged {
			// Try again soon, once the radio or TCP task has caught up.
			wake = wake.min(Instant::now() + Duration::from_millis(5));
		}
		let waited = Instant::now();
		let woken_by = next_wake(&mut receiver, wake).await;
		let woke = Instant::now();
		match woken_by {
			Wake::Frame(received) => {
				radio::record_signal(received.info.src_address, received.info.rx_control.rssi);
				node.handle_frame(received.info.src_address, received.data())
			}
			Wake::Timer => {}
			Wake::Uplink(message) => handle_uplink(&mut node, &mut stream, message),
			Wake::Lost(lost) => node.frame_lost(lost.mac, &lost.frame),
			Wake::Hotspot(frame) => overlay.receive_hotspot_frame(&mut node, &frame),
			#[cfg(feature = "ui")]
			Wake::Ui(UiInput::Command(Command::Save(new))) => {
				store.save_settings(&new);
				if settings.needs_restart(&new) {
					info!("Settings saved, restarting");
					esp_hal::system::software_reset();
				}
				apply_settings(&settings, &new, &mut node, &mut overlay);
				if new.channel != settings.channel && !bridge {
					let _ = manager.set_channel(new.channel);
				}
				settings = new;
				info!("Settings saved");
			}
			#[cfg(feature = "ui")]
			Wake::Ui(UiInput::Command(command)) => {
				carry_out(command, &mut node, &mut overlay);
			}
			#[cfg(feature = "voice")]
			Wake::Ui(UiInput::Voice(frame)) => call.send(&mut overlay, &frame, node.now()),
			#[cfg(feature = "ui")]
			Wake::Ui(UiInput::Networks(networks)) => {
				if !bridge {
					// Scanning hops channels, so return to the mesh's.
					let _ = manager.set_channel(settings.channel);
				}
				show_networks(networks);
			}
			// The pump below hands it more.
			#[cfg(feature = "sftp")]
			Wake::Ui(UiInput::SftpRoom) => {}
		}
		// Whatever woke the loop, run expired timers, or a steady stream of
		// frames would starve them.
		if node
			.poll_timeout()
			.is_some_and(|deadline| to_embassy(deadline) <= Instant::now())
		{
			node.handle_timeout();
		}
		#[cfg(feature = "ntp")]
		if let Some((unix_micros, stratum, received)) = ntp::WIFI_TIME.try_take() {
			let since = (Instant::now() - received).as_micros();
			overlay.set_time(unix_micros + since, stratum.saturating_add(1), node.now());
		}
		let board = BoardInfo {
			name: settings.name.clone(),
			model: model_name().into(),
			uptime: core::time::Duration::from_micros(Instant::now().as_micros()),
			free_heap: Some(esp_alloc::HEAP.free()),
			time: overlay.unix_micros(node.now()),
		};
		overlay.poll(&mut node, &board);
		while let Some(frame) = overlay.poll_hotspot_frame() {
			let _ = hotspot::TO_HOTSPOT.try_send(frame);
		}

		#[cfg(feature = "ui")]
		{
			while !ui::TO_UI.is_full() {
				let Some((key, info)) = node.poll_nodeinfo() else {
					break;
				};
				let _ = ui::TO_UI.try_send(ui::ToUi::NodeInfo(key, info));
			}
			// Only this task sends ordered UI messages; leave them queued when full.
			while !ui::TO_UI.is_full() {
				let Some(event) = overlay.poll_event() else {
					break;
				};
				#[cfg(feature = "sftp")]
				let Some(event) = sftp_pump.route(event) else {
					continue;
				};
				#[cfg(feature = "voice")]
				if let esp_ygg_app::Event::Call(event) = &event {
					call.handle_event(event);
				}
				let _ = ui::TO_UI.try_send(ui::ToUi::Event(event));
			}
			#[cfg(feature = "sftp")]
			sftp_pump.pump(&mut overlay);
			#[cfg(feature = "voice")]
			call.deliver(&mut overlay);

			if Instant::now() >= next_status {
				let uplink = uplink_state(bridge, &node, stream);
				send_status(&node, &board, uplink, overlay.hotspot_devices());
				next_status = Instant::now() + STATUS_INTERVAL;
			}
		}
		// Without a screen, the console's messages go to the log.
		#[cfg(not(feature = "ui"))]
		while let Some(_event) = overlay.poll_event() {
			#[cfg(feature = "console")]
			if let esp_ygg_app::Event::Datagram { from, data } = _event {
				info!(
					"Console message from {from}: {}",
					alloc::string::String::from_utf8_lossy(&data).trim_end()
				);
			}
		}

		let busy = (waited - started) + (Instant::now() - woke);
		if busy > STALL_WARNING {
			warn!("Main loop was busy for {} ms", busy.as_millis());
		}
		if Instant::now() >= next_save {
			store.save_time(unix_base + Instant::now().as_secs());
			next_save = Instant::now() + Duration::from_secs(SAVE_INTERVAL_SECS);
		}
	}
}

/// Hands the node's frames to the radio task, as many as it has room for.
fn send_frames(node: &mut Node<EspPlatform>, manager: &EspNowManager, frame: &mut [u8]) {
	while !radio::OUTBOX.is_full() {
		let Some((destination, len)) = node.poll_transmit(frame) else {
			break;
		};
		let mac = match destination {
			Destination::Broadcast => BROADCAST_ADDRESS,
			Destination::Unicast(mac) => {
				register_peer(manager, node, mac);
				mac
			}
		};
		let outgoing = radio::Outgoing {
			mac,
			frame: frame[..len].to_vec(),
		};
		let _ = radio::OUTBOX.try_send(outgoing);
	}
}

/// What the main loop woke for.
enum Wake {
	Frame(ReceivedData),
	Timer,
	Uplink(ToNode),
	/// A frame the radio could not deliver
	Lost(radio::Outgoing),
	Hotspot(Vec<u8>),
	#[cfg(feature = "ui")]
	Ui(UiInput),
}

async fn next_wake(receiver: &mut EspNowReceiver, at: Instant) -> Wake {
	let frame = async { Wake::Frame(receiver.receive_async().await) };
	let timer = async {
		Timer::at(at).await;
		Wake::Timer
	};
	let uplink = async { Wake::Uplink(TO_NODE.receive().await) };
	let lost = async { Wake::Lost(radio::LOST.receive().await) };
	let hotspot = async { Wake::Hotspot(hotspot::FROM_HOTSPOT.receive().await) };
	#[cfg(feature = "ui")]
	let ui = async { Wake::Ui(next_ui_input().await) };
	#[cfg(not(feature = "ui"))]
	let ui = core::future::pending::<Wake>();
	match select4(frame, timer, uplink, select3(lost, hotspot, ui)).await {
		Either4::First(wake)
		| Either4::Second(wake)
		| Either4::Third(wake)
		| Either4::Fourth(Either3::First(wake) | Either3::Second(wake) | Either3::Third(wake)) => wake,
	}
}

#[cfg(feature = "ui")]
enum UiInput {
	Command(Command),
	Networks(Vec<uplink::ScannedNetwork>),
	/// A frame from the microphone, for the call
	#[cfg(feature = "voice")]
	Voice(Vec<u8>),
	/// SFTP took what it was given, and has room for more.
	#[cfg(feature = "sftp")]
	SftpRoom,
}

/// A command from the UI, the results of a scan it asked for, or voice.
#[cfg(feature = "ui")]
async fn next_ui_input() -> UiInput {
	let command = async { UiInput::Command(ui::FROM_UI.receive().await) };
	let networks = async { UiInput::Networks(uplink::SCAN_RESULTS.receive().await) };
	#[cfg(feature = "voice")]
	let voice = async { UiInput::Voice(audio::VOICE_OUT.receive().await) };
	#[cfg(not(feature = "voice"))]
	let voice = core::future::pending::<UiInput>();
	#[cfg(feature = "sftp")]
	let voice = async {
		match embassy_futures::select::select(voice, sftp::ROOM.wait()).await {
			embassy_futures::select::Either::First(input) => input,
			embassy_futures::select::Either::Second(()) => UiInput::SftpRoom,
		}
	};
	match select3(command, networks, voice).await {
		Either3::First(input) | Either3::Second(input) | Either3::Third(input) => input,
	}
}

#[cfg(feature = "ui")]
fn show_networks(networks: Vec<uplink::ScannedNetwork>) {
	let networks = networks
		.into_iter()
		.map(|network| WifiNetwork {
			ssid: network.ssid,
			signal: network.signal,
			secured: network.secured,
		})
		.collect();
	ui::NETWORKS.signal(networks);
}

/// Gives the UI the node's state, with each neighbor's radio quality.
#[cfg(feature = "ui")]
fn send_status(
	node: &Node<EspPlatform>,
	board: &BoardInfo,
	uplink: Uplink,
	hotspot: Option<usize>,
) {
	let mut status = Status::from_node(node, board, uplink);
	status.hotspot = hotspot;
	for neighbor in &mut status.neighbors {
		(neighbor.signal, neighbor.loss) = radio::quality(&neighbor.mac);
	}
	let macs: Vec<Mac> = status
		.neighbors
		.iter()
		.map(|neighbor| neighbor.mac)
		.collect();
	radio::forget_others(&macs);
	ui::STATUS.signal(status);
}

#[cfg(feature = "ui")]
fn model_name() -> &'static str {
	ui::board::model_name()
}

/// Without an interface, the board is known only from how it was built.
#[cfg(not(feature = "ui"))]
fn model_name() -> &'static str {
	#[cfg(feature = "cardputer")]
	return cardputer::MODEL_NAME;
	#[cfg(feature = "c6-touch-lcd")]
	return touch_lcd::MODEL_NAME;
	#[allow(unreachable_code)]
	CHIP
}

#[cfg(feature = "ui")]
fn uplink_state(bridge: bool, node: &Node<EspPlatform>, stream: Option<StreamId>) -> Uplink {
	if !bridge {
		Uplink::Off
	} else if !uplink::WIFI_UP.load(core::sync::atomic::Ordering::Relaxed) {
		Uplink::JoiningWifi
	} else if stream.is_some_and(|id| node.stream_key(id).is_some()) {
		Uplink::Connected
	} else {
		Uplink::Connecting
	}
}

#[cfg(feature = "ui")]
fn carry_out(command: Command, node: &mut Node<EspPlatform>, overlay: &mut Overlay) {
	match command {
		Command::Ping(target) => {
			let _ = overlay.ping(target, node.now());
		}
		#[cfg(feature = "console")]
		Command::SendDatagram(to, data) => {
			if let Err(error) = overlay.send_datagram(to, &data) {
				warn!("Sending to {to} failed: {error:?}");
			}
		}
		Command::RequestNodeInfo(key) => node.request_nodeinfo(key),
		#[cfg(feature = "gopher")]
		Command::Fetch(to, request) => {
			if let Err(error) = overlay.fetch(to, request, node.now()) {
				warn!("Fetching from {to} failed: {error:?}");
			}
		}
		#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
		Command::OpenStream(stream, to) => {
			if let Err(error) = overlay.open_stream(stream, to) {
				warn!("Connecting to {to} failed: {error:?}");
			}
		}
		#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
		Command::StreamSend(stream, data) => {
			if let Err(error) = overlay.stream_send(stream, &data) {
				warn!("Sending on a stream failed: {error:?}");
			}
		}
		#[cfg(any(feature = "irc", feature = "gemini", feature = "sftp"))]
		Command::CloseStream(stream) => overlay.close_stream(stream),
		#[cfg(feature = "voice")]
		Command::Call(to) => {
			if let Err(error) = overlay.call(to, node.now()) {
				warn!("Calling {to} failed: {error:?}");
			}
		}
		#[cfg(feature = "voice")]
		Command::Answer => {
			let _ = overlay.answer(node.now());
		}
		#[cfg(feature = "voice")]
		Command::HangUp => {
			let _ = overlay.hang_up(node.now());
		}
		#[cfg(feature = "voice")]
		Command::PreviewRinger {
			tone,
			volume,
			samples,
		} => audio::preview_ringer(tone, volume, samples),
		Command::ScanWifi => {
			let _ = uplink::SCANS.try_send(());
		}
		// The main loop saves them, as it holds the settings in use.
		Command::Save(_) => {}
		// The UI's core reads and writes the SD card itself.
		Command::ExportSettings(_) | Command::ImportSettings => {}
		// The UI's side runs SFTP's sessions.
		#[cfg(feature = "sftp")]
		Command::SftpSend { .. }
		| Command::SftpAdmit(_)
		| Command::SftpPassword(_)
		| Command::SftpList => {}
	}
}

/// Settings from environment variables at build time, used until the user
/// Restarts the board once a panic has been printed. Otherwise it would sit
/// frozen, and the message is lost when nothing reads the serial port.
#[unsafe(no_mangle)]
extern "Rust" fn custom_halt() -> ! {
	esp_hal::system::software_reset()
}

/// saves some on the device.
fn build_time_settings() -> Settings {
	let mut settings = Settings::new();
	settings.name = option_env!("ESP_YGG_NAME").unwrap_or("").into();
	settings.ssid = option_env!("ESP_YGG_WIFI_SSID").unwrap_or("").into();
	settings.password = option_env!("ESP_YGG_WIFI_PASSWORD").unwrap_or("").into();
	settings.peer = option_env!("ESP_YGG_PEER").unwrap_or("").into();
	settings.bridge = settings.can_bridge();
	settings.status_page = option_env!("ESP_YGG_STATUS_PAGE") != Some("off");
	if let Some(name) = option_env!("ESP_YGG_HOTSPOT") {
		settings.hotspot = true;
		settings.hotspot_name = name.into();
		settings.hotspot_password = option_env!("ESP_YGG_HOTSPOT_PASSWORD").unwrap_or("").into();
	}
	settings.channel = option_env!("ESP_YGG_CHANNEL")
		.and_then(|channel| channel.parse().ok())
		.unwrap_or(DEFAULT_CHANNEL);
	settings
}

/// Applies the settings that need no restart, which the others' changes
/// would have.
#[cfg(feature = "ui")]
fn apply_settings(
	old: &Settings,
	new: &Settings,
	node: &mut Node<EspPlatform>,
	overlay: &mut Overlay,
) {
	if new.name != old.name {
		node.set_nodeinfo(nodeinfo(new));
		#[cfg(feature = "voice")]
		overlay.set_phone_name(phone_name(new));
	}
	if new.status_page != old.status_page {
		overlay.set_status_pages(new.status_page);
	}
	#[cfg(feature = "voice")]
	audio::set_ringer(new.ringtone, new.ring_volume);
	#[cfg(feature = "sftp")]
	if new.sftp_password != old.sftp_password {
		sftp::set_password(&new.sftp_password);
	}
}

/// The name calls go out under.
#[cfg(feature = "voice")]
fn phone_name(settings: &Settings) -> &str {
	if settings.name.is_empty() {
		"esp-ygg"
	} else {
		&settings.name
	}
}

fn node_config(store: &Store, settings: &Settings) -> Config {
	let mut config = Config::new(PrivateKey::from_seed(store.seed()));
	config.password = option_env!("ESP_YGG_PASSWORD").unwrap_or("").into();
	config.nodeinfo = nodeinfo(settings);
	config
}

/// What the node says about itself when asked: its name, if it has one,
/// and its build.
fn nodeinfo(settings: &Settings) -> Vec<u8> {
	let mut given = serde_json::Map::new();
	if !settings.name.is_empty() {
		given.insert("name".into(), settings.name.clone().into());
	}
	let build = BuildInfo {
		name: "esp-ygg",
		version: env!("CARGO_PKG_VERSION"),
		platform: CHIP,
		arch: if cfg!(target_arch = "xtensa") {
			"xtensa"
		} else {
			"riscv32"
		},
	};
	build_nodeinfo(&given, Some(build)).expect("building nodeinfo")
}

/// Adds `mac` to ESP-NOW's peer list, making room if needed.
fn register_peer(manager: &EspNowManager, node: &Node<EspPlatform>, mac: Mac) {
	if manager.peer_exists(&mac) {
		return;
	}
	let full = manager
		.peer_count()
		.is_ok_and(|count| count.total_count as usize >= MAX_ESPNOW_PEERS);
	if full {
		let neighbors: Vec<Mac> = node.neighbors().map(|(mac, _)| mac).collect();
		while let Ok(peer) = manager.fetch_peer(true) {
			if peer.peer_address != BROADCAST_ADDRESS && !neighbors.contains(&peer.peer_address) {
				let _ = manager.remove_peer(&peer.peer_address);
			}
			if manager
				.peer_count()
				.is_ok_and(|count| (count.total_count as usize) < MAX_ESPNOW_PEERS)
			{
				break;
			}
		}
	}
	let peer = PeerInfo {
		interface: EspNowWifiInterface::Station,
		peer_address: mac,
		lmk: None,
		channel: None,
		encrypt: false,
	};
	if let Err(error) = manager.add_peer(peer) {
		warn!("Adding ESP-NOW peer failed: {error:?}");
	}
}

fn handle_uplink(node: &mut Node<EspPlatform>, stream: &mut Option<StreamId>, message: ToNode) {
	match message {
		ToNode::Connected => {
			if let Some(old) = stream.replace(node.open_stream()) {
				node.close_stream(old);
			}
		}
		ToNode::Data(bytes) => {
			if let Some(id) = *stream
				&& node.stream_receive(id, &bytes).is_err()
			{
				let _ = TO_SOCKET.try_send(ToSocket::Close);
			}
		}
		ToNode::Disconnected => {
			if let Some(id) = stream.take() {
				node.close_stream(id);
			}
		}
	}
}

/// Passes bytes the node wants to send to the TCP task. Returns whether some
/// had to wait because the channel was full.
fn flush_stream(node: &mut Node<EspPlatform>, stream: &mut Option<StreamId>) -> bool {
	let Some(id) = *stream else {
		return false;
	};
	if !node.is_stream_open(id) {
		node.close_stream(id);
		*stream = None;
		let _ = TO_SOCKET.try_send(ToSocket::Close);
		return false;
	}
	loop {
		let pending = node.stream_transmit(id);
		if pending.is_empty() {
			return false;
		}
		let n = pending.len().min(CHUNK);
		if TO_SOCKET
			.try_send(ToSocket::Data(pending[..n].to_vec()))
			.is_err()
		{
			return true;
		}
		node.stream_consume(id, n);
	}
}

fn subnet_prefix(node: &Node<EspPlatform>) -> [u8; 16] {
	let mut prefix = [0u8; 16];
	prefix[..8].copy_from_slice(&node.subnet().0);
	prefix
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}
