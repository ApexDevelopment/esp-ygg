//! The time of day from an NTP server, while the board bridges over WiFi.

use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_deadline};
use esp_ygg_app::sntp;
use log::info;

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
	let mut token = [0; 8];
	esp_hal::rng::Rng::new().read(&mut token);
	let query = sntp::Query::new(
		core::net::SocketAddr::new(server.into(), sntp::PORT),
		u64::from_le_bytes(token),
	);
	socket
		.send_to(&query.request(), (server, sntp::PORT))
		.await
		.ok()?;
	let deadline = Instant::now() + Duration::from_secs(5);
	let mut reply = [0u8; 128];
	loop {
		let (length, metadata) = with_deadline(deadline, socket.recv_from(&mut reply))
			.await
			.ok()?
			.ok()?;
		let from =
			core::net::SocketAddr::new(metadata.endpoint.addr.into(), metadata.endpoint.port);
		if let Some(reply) = query.accept(from, &reply[..length]) {
			return Some((reply.unix_micros, reply.stratum, Instant::now()));
		}
	}
}
