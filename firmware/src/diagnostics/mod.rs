//! Optional serial controls, independent of the UI executor.

mod command;
mod packets;
pub mod wifi;
pub use packets::log_frames;

use alloc::string::String;
use core::net::SocketAddrV4;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use embassy_futures::select::{Either, select};
use embassy_net::{Stack, dns::DnsQueryType, tcp::TcpSocket};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_rtos::CurrentThreadHandle;
use log::{info, warn};

use command::{Command, WifiCommand};

pub static NODE_LOOPS: AtomicUsize = AtomicUsize::new(0);
pub static NETWORK_POLLS: AtomicUsize = AtomicUsize::new(0);
pub static UI_STARTED: AtomicBool = AtomicBool::new(false);
pub static UI_FRAMES: AtomicUsize = AtomicUsize::new(0);
pub static RADIO_TX_ENABLED: AtomicBool = AtomicBool::new(true);
static WATCH: AtomicBool = AtomicBool::new(true);
static WIFI_COMMANDS: Channel<CriticalSectionRawMutex, WifiCommand, 8> = Channel::new();
static PROBES: Channel<CriticalSectionRawMutex, Probe, 4> = Channel::new();

enum Probe {
	Status,
	Dns(String),
	Ntp,
	Tcp(Option<SocketAddrV4>),
}

pub fn command(text: &str) -> bool {
	if text.split_ascii_whitespace().next() != Some("diag") {
		return false;
	}
	match command::parse(text) {
		Ok(Command::Help) => help(),
		Ok(Command::Status) => {
			report();
			queue(&WIFI_COMMANDS, WifiCommand::Status);
			queue(&PROBES, Probe::Status);
		}
		Ok(Command::Watch(enabled)) => WATCH.store(enabled, Ordering::Relaxed),
		Ok(Command::Radio(enabled)) => {
			RADIO_TX_ENABLED.store(enabled, Ordering::Relaxed);
			info!("diag: ESP-NOW transmission enabled={enabled}");
		}
		Ok(Command::Pause(millis)) => {
			info!("diag: pausing network thread for {millis} ms");
			CurrentThreadHandle::get().delay(esp_hal::time::Duration::from_millis(millis.into()));
			info!("diag: network thread resumed");
		}
		Ok(Command::Priority(priority)) => {
			info!("diag: network priority={priority}; UI priority=1");
			CurrentThreadHandle::get().set_priority(priority);
		}
		Ok(Command::Reboot) => esp_hal::system::software_reset(),
		Ok(Command::Wifi(command)) => queue(&WIFI_COMMANDS, command),
		Ok(Command::Dns(host)) => queue(&PROBES, Probe::Dns(host.into())),
		Ok(Command::Ntp) => queue(&PROBES, Probe::Ntp),
		Ok(Command::Tcp(target)) => queue(&PROBES, Probe::Tcp(target)),
		Err(error) => warn!("diag: {error}"),
	}
	true
}

fn queue<T, const N: usize>(channel: &Channel<CriticalSectionRawMutex, T, N>, command: T) {
	if channel.try_send(command).is_err() {
		warn!("diag: command queue full; command rejected");
	} else {
		info!("diag: command queued");
	}
}

fn help() {
	for line in [
		"diag [status|help]; diag watch on|off; diag reboot",
		"diag radio on|off (ESP-NOW transmission)",
		"diag pause <1..1000 ms>; diag priority <0..3> (default 1)",
		"diag wifi [status|pause|resume|reconnect|reset]",
		"diag wifi scan [active|passive] [auto|1..13] [10..1000 ms]",
		"diag wifi power <8..84 quarter-dBm>; diag wifi ps none|min|max",
		"diag wifi protocol b|bg|bgn|bgnax; diag wifi bandwidth 20|40",
		"diag wifi bssid auto|aa:bb:cc:dd:ee:ff; diag wifi channel auto|1..13",
		"diag wifi method fast|all; diag wifi auth wpa2|mixed (WPA/WPA2)",
		"diag wifi sniff on|off; diag dns <hostname>; diag tcp [peer|IPv4:port]; diag ntp",
		"WiFi commands run between operations; connection and scan timeouts are 15 seconds.",
		"Diagnostic changes stay in RAM; reboot restores saved settings.",
	] {
		info!("{line}");
	}
}

fn report() {
	info!(
		"diag t={} ms: node loops={}, network polls={}, UI started={}, UI loops={}, free heap={}, UI queued={}, radio queued={}, capture dropped={}",
		Instant::now().as_millis(),
		NODE_LOOPS.load(Ordering::Relaxed),
		NETWORK_POLLS.load(Ordering::Relaxed),
		UI_STARTED.load(Ordering::Relaxed),
		UI_FRAMES.load(Ordering::Relaxed),
		esp_alloc::HEAP.free(),
		crate::ui::TO_UI.len(),
		crate::radio::OUTBOX.len(),
		packets::DROPPED.load(Ordering::Relaxed),
	);
}

fn network_status(stack: Option<Stack<'static>>, peer: Option<SocketAddrV4>) {
	match stack {
		Some(stack) => info!(
			"diag IP: link={}, configured={}, IPv4={:?}, IPv6={:?}, peer={peer:?}",
			stack.is_link_up(),
			stack.is_config_up(),
			stack.config_v4(),
			stack.config_v6()
		),
		None => info!(
			"diag IP: bridge disabled at boot; enable it in Settings and reboot to start DHCP/TCP"
		),
	}
}

#[embassy_executor::task]
pub async fn monitor(stack: Option<Stack<'static>>, peer: Option<SocketAddrV4>) {
	info!(
		"diag build={}, commands: diag help",
		env!("ESP_YGG_BUILD_TIME")
	);
	network_status(stack, peer);
	let mut next_report = Instant::now() + Duration::from_secs(5);
	loop {
		match select(PROBES.receive(), Timer::at(next_report)).await {
			Either::First(Probe::Status) => network_status(stack, peer),
			Either::First(probe) => {
				if let Some(stack) = stack.filter(|stack| stack.is_config_up()) {
					probe_network(stack, peer, probe).await;
				} else {
					warn!("diag: no IP configuration yet");
				}
			}
			Either::Second(()) => {
				if WATCH.load(Ordering::Relaxed) {
					report();
					network_status(stack, peer);
				}
				next_report = Instant::now() + Duration::from_secs(5);
			}
		}
	}
}

async fn probe_network(stack: Stack<'static>, peer: Option<SocketAddrV4>, probe: Probe) {
	let started = Instant::now();
	match probe {
		Probe::Dns(host) => {
			let result = with_timeout(
				Duration::from_secs(5),
				stack.dns_query(&host, DnsQueryType::A),
			)
			.await;
			info!(
				"diag DNS {host}: {result:?}, {} ms",
				started.elapsed().as_millis()
			);
		}
		Probe::Tcp(target) => {
			let Some(target) = target.or(peer) else {
				warn!("diag: no saved TCP peer");
				return;
			};
			let mut rx = [0; 512];
			let mut tx = [0; 512];
			let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
			let result = with_timeout(Duration::from_secs(5), socket.connect(target)).await;
			info!(
				"diag TCP {target}: {result:?}, {} ms",
				started.elapsed().as_millis()
			);
			socket.abort();
		}
		Probe::Ntp => {
			#[cfg(feature = "ntp")]
			{
				let result =
					with_timeout(Duration::from_secs(10), crate::ntp::query_ntp(stack)).await;
				info!("diag NTP: {result:?}, {} ms", started.elapsed().as_millis());
			}
			#[cfg(not(feature = "ntp"))]
			warn!("diag: NTP is not in this build");
		}
		Probe::Status => {}
	}
}
