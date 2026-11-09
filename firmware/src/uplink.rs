//! An optional peering over WiFi and TCP, for a node that bridges the ESP-NOW
//! mesh to the rest of the Yggdrasil network.
//!
//! The TCP connection runs in its own task and exchanges bytes with the node
//! through channels, so a slow connection never holds up the radio.

use alloc::vec::Vec;
use core::net::SocketAddrV4;

use embassy_futures::select::{Either, select};
use embassy_net::tcp::TcpSocket;
use embassy_net::{Runner, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write;
use esp_radio::wifi::{
	AuthenticationMethodConfig, Config, Interface, WifiController, sta::StationConfig,
};
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

pub struct Settings {
	pub ssid: &'static str,
	pub password: &'static str,
	pub peer: SocketAddrV4,
}

/// The uplink settings given at build time, if any.
pub fn settings() -> Option<Settings> {
	let ssid = option_env!("ESP_YGG_WIFI_SSID")?;
	let peer = option_env!("ESP_YGG_PEER")?;
	let Ok(peer) = peer.parse() else {
		panic!("ESP_YGG_PEER must be an IPv4 address and port, like 192.168.1.10:9001");
	};
	Some(Settings {
		ssid,
		password: option_env!("ESP_YGG_WIFI_PASSWORD").unwrap_or(""),
		peer,
	})
}

pub fn station_config(settings: &Settings) -> Config {
	let auth = if settings.password.is_empty() {
		AuthenticationMethodConfig::Open
	} else {
		AuthenticationMethodConfig::Wpa2Personal(
			settings.password.try_into().expect("password too long"),
		)
	};
	Config::Station(
		StationConfig::default()
			.with_ssid(settings.ssid.try_into().expect("SSID too long"))
			.with_authentication(auth),
	)
}

#[embassy_executor::task]
pub async fn wifi(mut controller: WifiController<'static>) {
	loop {
		match controller.connect_async().await {
			Ok(_) => {
				info!("WiFi connected");
				let _ = controller.wait_for_disconnect_async().await;
				warn!("WiFi disconnected");
			}
			Err(error) => warn!("WiFi connection failed: {error:?}"),
		}
		Timer::after(RETRY_DELAY).await;
	}
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
