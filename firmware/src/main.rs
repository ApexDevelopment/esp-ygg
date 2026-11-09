#![no_std]
#![no_main]

extern crate alloc;

mod platform;
mod radio;
mod storage;
mod uplink;

use alloc::vec::Vec;
use core::net::Ipv6Addr;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_net::StackResources;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{BROADCAST_ADDRESS, EspNowManager, EspNowWifiInterface, PeerInfo};
use esp_radio::wifi::{ControllerConfig, WifiController};
use esp_storage::FlashStorage;
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
	#[cfg(not(feature = "esp32"))]
	esp_alloc::heap_allocator!(size: 160 * 1024);
	#[cfg(feature = "esp32")]
	esp_alloc::heap_allocator!(size: 120 * 1024);
	let timg0 = TimerGroup::new(peripherals.TIMG0);
	esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

	let uplink = uplink::settings();
	let mut controller_config = ControllerConfig::default();
	if let Some(settings) = &uplink {
		controller_config = controller_config.with_initial_config(uplink::station_config(settings));
	}
	let controller =
		WifiController::new(peripherals.WIFI, controller_config).expect("starting WiFi");
	// The radio is running now, so the random number generator is truly random.
	let rng = Rng::new();
	let (mut store, unix_base) = Store::open(FlashStorage::new(peripherals.FLASH), &rng);

	let esp_now = controller.esp_now();
	info!("ESP-NOW version {}", esp_now.version().unwrap_or(0));
	if uplink.is_none() {
		let channel = option_env!("ESP_YGG_CHANNEL")
			.and_then(|channel| channel.parse().ok())
			.unwrap_or(DEFAULT_CHANNEL);
		esp_now
			.set_channel(channel)
			.expect("setting the WiFi channel");
		info!("Using WiFi channel {channel}");
	}
	let (manager, sender, mut receiver) = esp_now.split();
	spawner.spawn(radio::sender(sender).expect("spawning the radio task"));

	if let Some(settings) = uplink {
		let (stack, runner) = embassy_net::new(
			esp_radio::wifi::Interface::station(),
			embassy_net::Config::dhcpv4(Default::default()),
			{
				static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
				RESOURCES.init(StackResources::new())
			},
			u64::from(rng.random()) << 32 | u64::from(rng.random()),
		);
		spawner.spawn(uplink::wifi(controller).expect("spawning the WiFi task"));
		spawner.spawn(uplink::network(runner).expect("spawning the network task"));
		spawner.spawn(uplink::tcp(stack, settings.peer).expect("spawning the TCP task"));
		info!(
			"Peering with {} over WiFi network {}",
			settings.peer, settings.ssid
		);
	} else {
		// ESP-NOW keeps the radio running on its own.
		drop(controller);
	}

	let mut node = Node::new(node_config(&store), EspPlatform::new(rng, unix_base))
		.expect("starting the Yggdrasil node");
	info!("Public key: {}", Hex(&node.public_key().0));
	info!("Address: {}", Ipv6Addr::from(node.address().0));
	info!("Subnet: {}/64", Ipv6Addr::from(subnet_prefix(&node)));

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

		let mut wake = node
			.poll_timeout()
			.map(to_embassy)
			.unwrap_or_else(|| Instant::now() + Duration::from_secs(1))
			.min(next_save);
		if backlogged {
			// Try again soon, once the radio or TCP task has caught up.
			wake = wake.min(Instant::now() + Duration::from_millis(5));
		}
		let waited = Instant::now();
		let event = select3(receiver.receive_async(), Timer::at(wake), TO_NODE.receive()).await;
		let woke = Instant::now();
		match event {
			Either3::First(received) => {
				node.handle_frame(received.info.src_address, received.data())
			}
			Either3::Second(()) => {}
			Either3::Third(message) => handle_uplink(&mut node, &mut stream, message),
		}
		// Whatever woke the loop, run expired timers, or a steady stream of
		// frames would starve them.
		if node
			.poll_timeout()
			.is_some_and(|deadline| to_embassy(deadline) <= Instant::now())
		{
			node.handle_timeout();
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

fn node_config(store: &Store) -> Config {
	let mut config = Config::new(PrivateKey::from_seed(store.seed()));
	config.password = option_env!("ESP_YGG_PASSWORD").unwrap_or("").into();
	let mut given = serde_json::Map::new();
	if let Some(name) = option_env!("ESP_YGG_NAME") {
		given.insert("name".into(), name.into());
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
