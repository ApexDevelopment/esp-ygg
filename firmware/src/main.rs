#![no_std]
#![no_main]

extern crate alloc;

#[cfg(feature = "voice")]
mod audio;
#[cfg(feature = "cardputer")]
mod cardputer;
mod hotspot;
#[cfg(feature = "ntp")]
mod ntp;
mod park;
mod platform;
mod radio;
mod storage;
#[cfg(feature = "cardputer")]
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
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowManager, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, ControllerConfig, Interface, WifiController};
use esp_storage::FlashStorage;
#[cfg(feature = "cardputer")]
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
#[cfg(feature = "cardputer")]
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
	let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
	esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
	// The original ESP32 has less RAM to spare
	#[cfg(not(any(feature = "esp32", feature = "cardputer")))]
	esp_alloc::heap_allocator!(size: 200 * 1024);
	// The UI's core has a stack of its own, so leave room for it.
	#[cfg(feature = "cardputer")]
	esp_alloc::heap_allocator!(size: 176 * 1024);
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
	let settings = settings_store
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
	let controller =
		WifiController::new(peripherals.WIFI, controller_config).expect("starting WiFi");
	let rng = Rng::new();
	let (mut store, unix_base) = settings_store.open(&rng);

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
		let name = if settings.name.is_empty() {
			"esp-ygg"
		} else {
			&settings.name
		};
		overlay.start_phone(name, rng.random() as u16);
	}
	#[cfg(feature = "voice")]
	let mut ringing = false;
	// Voice frames sent and received in the current call
	#[cfg(feature = "voice")]
	let mut voice_frames = (0u32, 0u32);
	// Warn about dropped voice once per call
	#[cfg(feature = "voice")]
	let mut voice_overflowed = false;
	if hotspot_on {
		let access_point = Interface::access_point();
		overlay.start_hotspot(&node, access_point.mac_address());
		spawner.spawn(hotspot::link(access_point).expect("spawning the hotspot task"));
		info!("Hotspot {} is on", settings.hotspot_name);
	}

	#[cfg(feature = "cardputer")]
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
		};
		ui::start(
			peripherals.CPU_CTRL,
			(peripherals.FROM_CPU_INTR1, peripherals.FROM_CPU_INTR3),
			pins,
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
	#[cfg(feature = "cardputer")]
	let mut next_status = Instant::now();

	let mut frame = alloc::vec![0u8; FRAME_LEN];
	let mut stream: Option<StreamId> = None;
	let mut next_save = Instant::now() + Duration::from_secs(SAVE_INTERVAL_SECS);
	loop {
		let started = Instant::now();
		while !radio::OUTBOX.is_full() {
			let Some((destination, len)) = node.poll_transmit(&mut frame) else {
				break;
			};
			let mac = match destination {
				Destination::Broadcast => BROADCAST_ADDRESS,
				Destination::Unicast(mac) => {
					register_peer(&manager, &node, mac);
					mac
				}
			};
			let outgoing = radio::Outgoing {
				mac,
				frame: frame[..len].to_vec(),
			};
			let _ = radio::OUTBOX.try_send(outgoing);
		}
		let backlogged = flush_stream(&mut node, &mut stream) || radio::OUTBOX.is_full();

		let mut wake = [
			node.poll_timeout().map(to_embassy),
			overlay.poll_at(node.now()).map(to_embassy),
		]
		.into_iter()
		.flatten()
		.min()
		.unwrap_or_else(|| Instant::now() + Duration::from_secs(1))
		.min(next_save);
		#[cfg(feature = "cardputer")]
		{
			wake = wake.min(next_status);
		}
		if backlogged {
			// Try again soon, once the radio or TCP task has caught up.
			wake = wake.min(Instant::now() + Duration::from_millis(5));
		}
		let waited = Instant::now();
		let event = select4(
			receiver.receive_async(),
			Timer::at(wake),
			TO_NODE.receive(),
			select3(
				radio::LOST.receive(),
				hotspot::FROM_HOTSPOT.receive(),
				next_ui_input(),
			),
		)
		.await;
		let woke = Instant::now();
		let cause = cause(&event);
		match event {
			Either4::First(received) => {
				radio::record_signal(received.info.src_address, received.info.rx_control.rssi);
				node.handle_frame(received.info.src_address, received.data())
			}
			Either4::Second(()) => {}
			Either4::Third(message) => handle_uplink(&mut node, &mut stream, message),
			Either4::Fourth(Either3::First(lost)) => node.frame_lost(lost.mac, &lost.frame),
			Either4::Fourth(Either3::Second(frame)) => {
				overlay.receive_hotspot_frame(&mut node, &frame);
			}
			#[cfg(feature = "cardputer")]
			Either4::Fourth(Either3::Third(UiInput::Command(command))) => {
				carry_out(command, &mut node, &mut overlay, &mut store);
			}
			#[cfg(feature = "cardputer")]
			Either4::Fourth(Either3::Third(UiInput::Voice(_frame))) => {
				#[cfg(feature = "voice")]
				{
					overlay.send_voice(&_frame, node.now());
					voice_frames.0 += 1;
				}
			}
			#[cfg(feature = "cardputer")]
			Either4::Fourth(Either3::Third(UiInput::Networks(networks))) => {
				if !bridge {
					// Scanning hops channels, so return to the mesh's.
					let _ = manager.set_channel(settings.channel);
				}
				let networks = networks
					.into_iter()
					.map(|network| WifiNetwork {
						ssid: network.ssid,
						signal: network.signal,
						secured: network.secured,
					})
					.collect();
				let _ = ui::TO_UI.try_send(ui::ToUi::Networks(networks));
			}
			#[cfg(not(feature = "cardputer"))]
			Either4::Fourth(Either3::Third(never)) => match never {},
		}
		let handled = Instant::now();
		// Whatever woke the loop, run expired timers, or a steady stream of
		// frames would starve them.
		if node
			.poll_timeout()
			.is_some_and(|deadline| to_embassy(deadline) <= Instant::now())
		{
			node.handle_timeout();
		}
		let timers_run = Instant::now();
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
		let services_run = Instant::now();
		while let Some(frame) = overlay.poll_hotspot_frame() {
			let _ = hotspot::TO_HOTSPOT.try_send(frame);
		}

		#[cfg(feature = "cardputer")]
		{
			// If the UI falls behind, it misses some updates rather than
			// holding up the network.
			while let Some(event) = overlay.poll_event() {
				#[cfg(feature = "voice")]
				if let esp_ygg_app::Event::Call(event) = &event {
					use esp_ygg_app::voice::iax2::CallEvent;
					match event {
						CallEvent::Incoming { .. } => ringing = true,
						CallEvent::Answered => ringing = false,
						CallEvent::Ended(reason) => {
							ringing = false;
							let (sent, received) = core::mem::take(&mut voice_frames);
							voice_overflowed = false;
							info!(
								"Call ended ({reason}): {sent} voice frames sent, {received} received"
							);
						}
						CallEvent::Ringing => {}
					}
				}
				let _ = ui::TO_UI.try_send(ui::ToUi::Event(event));
			}
			#[cfg(feature = "voice")]
			{
				while let Some(voice) = overlay.poll_voice() {
					voice_frames.1 += 1;
					if audio::VOICE_IN.try_send(voice).is_err() && !voice_overflowed {
						voice_overflowed = true;
						warn!("Voice arrived faster than it played, so some was dropped");
					}
				}
				audio::set_call(ringing, overlay.call_codec());
			}
			while let Some((key, info)) = node.poll_nodeinfo() {
				let _ = ui::TO_UI.try_send(ui::ToUi::NodeInfo(key, info));
			}
			if Instant::now() >= next_status {
				let uplink = uplink_state(bridge, &node, stream);
				let mut status = Status::from_node(&node, &board, uplink);
				status.hotspot = overlay.hotspot_devices();
				for neighbor in &mut status.neighbors {
					(neighbor.signal, neighbor.loss) = radio::quality(&neighbor.mac);
				}
				let macs: Vec<Mac> = status
					.neighbors
					.iter()
					.map(|neighbor| neighbor.mac)
					.collect();
				radio::forget_others(&macs);
				let _ = ui::TO_UI.try_send(ui::ToUi::Status(status));
				next_status = Instant::now() + STATUS_INTERVAL;
			}
		}
		// Without a screen, the console's messages go to the log.
		#[cfg(not(feature = "cardputer"))]
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
			warn!(
				"Main loop was busy for {} ms: {} before waiting, {} handling {cause}, {} on timers, {} on services",
				busy.as_millis(),
				(waited - started).as_millis(),
				(handled - woke).as_millis(),
				(timers_run - handled).as_millis(),
				(services_run - timers_run).as_millis(),
			);
		}
		if Instant::now() >= next_save {
			store.save_time(unix_base + Instant::now().as_secs());
			next_save = Instant::now() + Duration::from_secs(SAVE_INTERVAL_SECS);
		}
	}
}

/// What woke the main loop, for the stall warning.
fn cause<A, B, C, D, E, F>(event: &Either4<A, B, C, Either3<D, E, F>>) -> &'static str {
	match event {
		Either4::First(_) => "a radio frame",
		Either4::Second(_) => "a timer",
		Either4::Third(_) => "the bridge",
		Either4::Fourth(Either3::First(_)) => "a lost frame",
		Either4::Fourth(Either3::Second(_)) => "a hotspot frame",
		Either4::Fourth(Either3::Third(_)) => "the UI",
	}
}

#[cfg(feature = "cardputer")]
enum UiInput {
	Command(Command),
	Networks(Vec<uplink::ScannedNetwork>),
	/// A frame from the microphone, for the call
	Voice(Vec<u8>),
}

/// A command from the UI, the results of a scan it asked for, or voice.
#[cfg(feature = "cardputer")]
async fn next_ui_input() -> UiInput {
	match select3(
		ui::FROM_UI.receive(),
		uplink::SCAN_RESULTS.receive(),
		next_voice(),
	)
	.await
	{
		Either3::First(command) => UiInput::Command(command),
		Either3::Second(networks) => UiInput::Networks(networks),
		Either3::Third(frame) => UiInput::Voice(frame),
	}
}

#[cfg(feature = "voice")]
async fn next_voice() -> Vec<u8> {
	audio::VOICE_OUT.receive().await
}

#[cfg(all(feature = "cardputer", not(feature = "voice")))]
async fn next_voice() -> Vec<u8> {
	core::future::pending().await
}

#[cfg(feature = "cardputer")]
fn model_name() -> &'static str {
	ui::model_name()
}

#[cfg(not(feature = "cardputer"))]
fn model_name() -> &'static str {
	CHIP
}

/// Without a UI there is no input, so this never finishes.
#[cfg(not(feature = "cardputer"))]
async fn next_ui_input() -> core::convert::Infallible {
	core::future::pending().await
}

#[cfg(feature = "cardputer")]
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

#[cfg(feature = "cardputer")]
fn carry_out(
	command: Command,
	node: &mut Node<EspPlatform>,
	overlay: &mut Overlay,
	store: &mut Store,
) {
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
		#[cfg(any(feature = "irc", feature = "gemini"))]
		Command::OpenStream(stream, to) => {
			if let Err(error) = overlay.open_stream(stream, to) {
				warn!("Connecting to {to} failed: {error:?}");
			}
		}
		#[cfg(any(feature = "irc", feature = "gemini"))]
		Command::StreamSend(stream, data) => {
			if let Err(error) = overlay.stream_send(stream, &data) {
				warn!("Sending on a stream failed: {error:?}");
			}
		}
		#[cfg(any(feature = "irc", feature = "gemini"))]
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
		Command::ScanWifi => {
			let _ = uplink::SCANS.try_send(());
		}
		Command::Save(settings) => {
			store.save_settings(&settings);
			info!("Settings saved, restarting");
			esp_hal::system::software_reset();
		}
	}
}

/// Settings from environment variables at build time, used until the user
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

fn node_config(store: &Store, settings: &Settings) -> Config {
	let mut config = Config::new(PrivateKey::from_seed(store.seed()));
	config.password = option_env!("ESP_YGG_PASSWORD").unwrap_or("").into();
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
	config.nodeinfo = build_nodeinfo(&given, Some(build)).expect("building nodeinfo");
	config
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
