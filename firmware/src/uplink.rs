//! An optional peering over WiFi and TCP, for a node that bridges the ESP-NOW
//! mesh to the rest of the Yggdrasil network.
//!
//! The TCP connection runs in its own task and exchanges bytes with the node
//! through channels, so a slow connection never holds up the radio.

use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddrV4;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Runner, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::{
	AuthenticationMethod, AuthenticationMethodConfig, Config, Interface, WifiController,
	sta::StationConfig,
};
use esp_ygg_app::sntp;
use log::{info, warn};

const RETRY_DELAY: Duration = Duration::from_secs(5);
/// The largest chunk passed through a channel.
pub const CHUNK: usize = 1024;

pub enum ToNode {
	Connected,
	Data(Vec<u8>),
	Disconnected,
}

pub enum ToSocket {
	Data(Vec<u8>),
	Close,
}

pub static TO_NODE: Channel<CriticalSectionRawMutex, ToNode, 8> = Channel::new();
pub static TO_SOCKET: Channel<CriticalSectionRawMutex, ToSocket, 8> = Channel::new();

/// Set while the board is connected to a WiFi network.
pub static WIFI_UP: AtomicBool = AtomicBool::new(false);
/// Asks the WiFi task to scan for networks.
pub static SCANS: Channel<CriticalSectionRawMutex, (), 1> = Channel::new();
pub static SCAN_RESULTS: Channel<CriticalSectionRawMutex, Vec<ScannedNetwork>, 1> = Channel::new();

// Only boards with a UI ask for scans.
#[cfg_attr(not(feature = "cardputer"), allow(dead_code))]
pub struct ScannedNetwork {
	pub ssid: String,
	pub signal: i8,
	pub secured: bool,
}

pub fn station_config(ssid: &str, password: &str) -> Option<Config> {
	let auth = if password.is_empty() {
		AuthenticationMethodConfig::Open
	} else {
		AuthenticationMethodConfig::Wpa2Personal(password.try_into().ok()?)
	};
	Some(Config::Station(
		StationConfig::default()
			.with_ssid(ssid.try_into().ok()?)
			.with_authentication(auth),
	))
}

/// Owns the WiFi controller: keeps the board on its network when bridging,
/// and scans for networks when asked.
#[embassy_executor::task]
pub async fn wifi(mut controller: WifiController<'static>, bridge: bool) {
	loop {
		if !bridge {
			SCANS.receive().await;
			scan(&mut controller).await;
			continue;
		}
		match controller.connect_async().await {
			Ok(_) => {
				info!("WiFi connected");
				WIFI_UP.store(true, Ordering::Relaxed);
				loop {
					match select(controller.wait_for_disconnect_async(), SCANS.receive()).await {
						Either::First(_) => break,
						Either::Second(()) => scan(&mut controller).await,
					}
				}
				WIFI_UP.store(false, Ordering::Relaxed);
				warn!("WiFi disconnected");
			}
			Err(error) => warn!("WiFi connection failed: {error:?}"),
		}
		if let Either::Second(()) = select(Timer::after(RETRY_DELAY), SCANS.receive()).await {
			scan(&mut controller).await;
		}
	}
}

async fn scan(controller: &mut WifiController<'static>) {
	let config = ScanConfig::default().with_max(20);
	let networks = match controller.scan_async(&config).await {
		Ok(found) => found
			.into_iter()
			.filter(|network| !network.ssid.is_empty())
			.map(|network| ScannedNetwork {
				ssid: network.ssid.as_str().into(),
				signal: network.signal_strength,
				secured: !matches!(network.auth_method, None | Some(AuthenticationMethod::None)),
			})
			.collect(),
		Err(error) => {
			warn!("WiFi scan failed: {error:?}");
			Vec::new()
		}
	};
	let _ = SCAN_RESULTS.try_send(networks);
}

#[embassy_executor::task]
pub async fn network(mut runner: Runner<'static, Interface>) {
	runner.run().await
}

#[embassy_executor::task]
pub async fn tcp(stack: Stack<'static>, peer: SocketAddrV4) {
	let mut rx_buffer = [0u8; 4096];
	let mut tx_buffer = [0u8; 4096];
	loop {
		stack.wait_config_up().await;
		let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
		socket.set_timeout(Some(Duration::from_secs(30)));
		socket.set_keep_alive(Some(Duration::from_secs(10)));
		// Small writes such as keepalives must not wait behind unacknowledged
		// data, or a lost ACK can hold them past the peer's 3 s timeout.
		socket.set_nagle_enabled(false);
		if let Err(error) = socket.connect(peer).await {
			warn!("Connecting to {peer} failed: {error:?}");
			Timer::after(RETRY_DELAY).await;
			continue;
		}
		info!("Connected to {peer}");
		// Anything left over was meant for the previous connection
		while TO_SOCKET.try_receive().is_ok() {}
		TO_NODE.send(ToNode::Connected).await;
		relay(&mut socket).await;
		socket.abort();
		let _ = socket.flush().await;
		warn!("Disconnected from {peer}");
		TO_NODE.send(ToNode::Disconnected).await;
		Timer::after(RETRY_DELAY).await;
	}
}

/// Passes bytes between the socket and the node until either side closes.
async fn relay(socket: &mut TcpSocket<'_>) {
	let mut buf = [0u8; CHUNK];
	loop {
		match select(socket.read(&mut buf), TO_SOCKET.receive()).await {
			Either::First(Ok(0) | Err(_)) => return,
			Either::First(Ok(n)) => TO_NODE.send(ToNode::Data(buf[..n].to_vec())).await,
			Either::Second(ToSocket::Data(bytes)) => {
				if socket.write_all(&bytes).await.is_err() {
					return;
				}
			}
			Either::Second(ToSocket::Close) => return,
		}
	}
}

/// The time from an NTP server over WiFi: microseconds since the Unix epoch,
/// the server's stratum, and when the reply arrived.
pub static WIFI_TIME: Signal<CriticalSectionRawMutex, (u64, u8, Instant)> = Signal::new();

const NTP_SERVER: &str = "pool.ntp.org";
const NTP_RETRY: Duration = Duration::from_secs(30);
const NTP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Keeps the board's clock set from an NTP server while bridging.
#[embassy_executor::task]
pub async fn ntp(stack: Stack<'static>) {
	loop {
		stack.wait_config_up().await;
		match query_ntp(stack).await {
			Some(time) => {
				info!("Got the time from {NTP_SERVER}");
				WIFI_TIME.signal(time);
				Timer::after(NTP_INTERVAL).await;
			}
			None => Timer::after(NTP_RETRY).await,
		}
	}
}

async fn query_ntp(stack: Stack<'static>) -> Option<(u64, u8, Instant)> {
	let addresses = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
	let server = *addresses.first()?;
	let (mut rx_meta, mut tx_meta) = ([PacketMetadata::EMPTY; 1], [PacketMetadata::EMPTY; 1]);
	let (mut rx_buffer, mut tx_buffer) = ([0u8; 128], [0u8; 128]);
	let mut socket = UdpSocket::new(
		stack,
		&mut rx_meta,
		&mut rx_buffer,
		&mut tx_meta,
		&mut tx_buffer,
	);
	socket.bind(0).ok()?;
	socket
		.send_to(&sntp::request(), (server, sntp::PORT))
		.await
		.ok()?;
	let mut reply = [0u8; 128];
	let (length, _) = with_timeout(Duration::from_secs(5), socket.recv_from(&mut reply))
		.await
		.ok()?
		.ok()?;
	let received = Instant::now();
	match sntp::parse(&reply[..length])? {
		sntp::Message::Reply(reply) => Some((reply.unix_micros, reply.stratum, received)),
		sntp::Message::Request => None,
	}
}
