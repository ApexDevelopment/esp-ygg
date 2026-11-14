//! Wi-Fi experiments using the saved credentials, without changing NVS.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::{
	AuthenticationMethodConfig, Bandwidth, Bandwidths, Config, PowerSaveMode, Protocol, Protocols,
	WifiController, WifiError,
	scan::{ScanConfig, ScanTypeConfig},
	sniffer::Sniffer,
	sta::{ScanMethod, StationConfig},
};
use log::{info, warn};

use super::{
	WIFI_COMMANDS,
	command::{PowerSave, Protocols as ProtocolChoice, WifiCommand},
	packets,
};
use crate::uplink::{self, WIFI_UP};
use core::sync::atomic::Ordering;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

struct State {
	mac: [u8; 6],
	saved: Config,
	current: Config,
	enabled: bool,
	saved_enabled: bool,
	requested_power: i8,
	power_save: PowerSaveMode,
	bandwidths: Option<Bandwidths>,
	sniffer: Option<Sniffer>,
}

#[embassy_executor::task]
pub async fn run(
	mut controller: WifiController<'static>,
	config: Config,
	enabled: bool,
	mac: [u8; 6],
) {
	let mut state = State {
		mac,
		saved: config.clone(),
		current: config,
		enabled,
		saved_enabled: enabled,
		requested_power: 20,
		power_save: PowerSaveMode::None,
		bandwidths: controller.bandwidths().ok(),
		sniffer: None,
	};
	info!("diag: WiFi country at startup=CN (driver default); all overrides are temporary");
	status(&controller, &state);
	loop {
		if let Ok(command) = WIFI_COMMANDS.try_receive() {
			handle(&mut controller, &mut state, command).await;
			continue;
		}
		if !state.enabled {
			let command = next_command().await;
			handle(&mut controller, &mut state, command).await;
		} else if controller.is_connected() {
			WIFI_UP.store(true, Ordering::Relaxed);
			match select(controller.wait_for_disconnect_async(), next_command()).await {
				Either::First(result) => {
					WIFI_UP.store(false, Ordering::Relaxed);
					warn!(
						"diag t={} ms: disconnected {result:?}",
						Instant::now().as_millis()
					);
				}
				Either::Second(command) => handle(&mut controller, &mut state, command).await,
			}
		} else {
			connect(&mut controller).await;
			if !controller.is_connected() {
				if let Either::Second(command) =
					select(Timer::after(Duration::from_secs(5)), next_command()).await
				{
					handle(&mut controller, &mut state, command).await;
				}
			}
		}
	}
}

async fn next_command() -> WifiCommand {
	match select(WIFI_COMMANDS.receive(), uplink::SCANS.receive()).await {
		Either::First(command) => command,
		Either::Second(()) => WifiCommand::Scan {
			active: false,
			channel: None,
			dwell: 360,
		},
	}
}

async fn connect(controller: &mut WifiController<'static>) {
	let started = Instant::now();
	info!("diag t={} ms: connecting", started.as_millis());
	match with_timeout(OPERATION_TIMEOUT, controller.connect_async()).await {
		Ok(Ok(connection)) => {
			WIFI_UP.store(true, Ordering::Relaxed);
			info!(
				"diag: connected {connection:?}, took {} ms",
				started.elapsed().as_millis()
			);
		}
		result => {
			warn!(
				"diag: connection failed {result:?}, took {} ms",
				started.elapsed().as_millis()
			);
			disconnect(controller).await;
		}
	}
}

async fn disconnect(controller: &mut WifiController<'static>) {
	WIFI_UP.store(false, Ordering::Relaxed);
	match with_timeout(Duration::from_secs(3), controller.disconnect_async()).await {
		Ok(Ok(result)) => info!("diag: disconnect {result:?}"),
		Ok(Err(WifiError::NotConnected)) => {}
		result => warn!("diag: disconnect failed {result:?}"),
	}
}

async fn handle(controller: &mut WifiController<'static>, state: &mut State, command: WifiCommand) {
	info!("diag WiFi command: {command:?}");
	match command {
		WifiCommand::Status => status(controller, state),
		WifiCommand::Enabled(enabled) => {
			state.enabled = enabled;
			if !enabled {
				disconnect(controller).await;
			}
		}
		WifiCommand::Reconnect => {
			disconnect(controller).await;
			state.enabled = true;
		}
		WifiCommand::Reset => reset(controller, state).await,
		WifiCommand::Scan {
			active,
			channel,
			dwell,
		} => {
			let dwell = esp_hal::time::Duration::from_millis(dwell.into());
			let scan_type = if active {
				ScanTypeConfig::Active {
					min: dwell,
					max: dwell,
				}
			} else {
				ScanTypeConfig::Passive(dwell)
			};
			let mut config = ScanConfig::default()
				.with_show_hidden(true)
				.with_scan_type(scan_type);
			if let Some(channel) = channel {
				config = config.with_channel(channel);
			}
			let started = Instant::now();
			let result =
				with_timeout(OPERATION_TIMEOUT, uplink::scan_with(controller, &config)).await;
			info!(
				"diag: scan finished {result:?}, took {} ms",
				started.elapsed().as_millis()
			);
		}
		WifiCommand::Power(power) => match controller.set_max_tx_power(power) {
			Ok(()) => {
				state.requested_power = power;
				info!("diag: requested TX power={power} quarter-dBm");
			}
			Err(error) => warn!("diag: setting TX power failed: {error:?}"),
		},
		WifiCommand::PowerSave(mode) => {
			let mode = match mode {
				PowerSave::None => PowerSaveMode::None,
				PowerSave::Min => PowerSaveMode::Minimum,
				PowerSave::Max => PowerSaveMode::Maximum,
			};
			match controller.set_power_saving(mode) {
				Ok(()) => state.power_save = mode,
				Err(error) => warn!("diag: setting power saving failed: {error:?}"),
			}
		}
		WifiCommand::Bandwidth(mhz) => {
			let result = controller.bandwidths().and_then(|widths| {
				controller.set_bandwidths(widths.with_2_4(if mhz == 20 {
					Bandwidth::_20MHz
				} else {
					Bandwidth::_40MHz
				}))
			});
			info!("diag: bandwidth request {mhz} MHz: {result:?}");
		}
		WifiCommand::Sniff(enabled) => sniff(controller, state, enabled),
		command => change_station(controller, state, command).await,
	}
}

fn station(config: &Config) -> Option<&StationConfig> {
	match config {
		Config::Station(station) | Config::AccessPointStation(station, _) => Some(station),
		_ => None,
	}
}

fn status(controller: &WifiController<'static>, state: &State) {
	info!(
		"diag WiFi: autojoin={}, connected={}, MAC={:02x?}, RSSI={:?}, channel={:?}, bandwidth={:?}, requested power={} quarter-dBm, power saving={:?}, capture={}",
		state.enabled,
		controller.is_connected(),
		state.mac,
		controller.rssi(),
		controller.channel(),
		controller.bandwidths(),
		state.requested_power,
		state.power_save,
		state.sniffer.is_some()
	);
	if let Some(station) = station(&state.current) {
		let auth = match station.authentication() {
			AuthenticationMethodConfig::Open => "open",
			AuthenticationMethodConfig::Wpa2Personal(_) => "WPA2",
			AuthenticationMethodConfig::WpaWpa2Personal(_) => "WPA/WPA2",
			_ => "other",
		};
		info!(
			"diag station: SSID={:?}, auth={auth}, BSSID={:02x?}, channel={:?}, protocols={:?}, scan={:?}, retries={}",
			station.ssid(),
			station.bssid(),
			station.channel(),
			station.protocols(),
			station.scan_method(),
			station.failure_retry_cnt()
		);
	}
	if controller.is_connected() {
		info!("diag connected AP: {:?}", controller.ap_info());
	}
}

async fn change_station(
	controller: &mut WifiController<'static>,
	state: &mut State,
	command: WifiCommand,
) {
	let Some(current) = station(&state.current) else {
		warn!("diag: no station configuration");
		return;
	};
	let mut updated = current.clone();
	match command {
		WifiCommand::Bssid(Some(mac)) => updated = updated.with_bssid(mac),
		WifiCommand::Bssid(None) => updated = updated.with_bssid_none(),
		WifiCommand::Channel(Some(channel)) => updated = updated.with_channel(channel),
		WifiCommand::Channel(None) => updated = updated.with_channel_none(),
		WifiCommand::AllChannels(all) => {
			updated = updated.with_scan_method(if all {
				ScanMethod::AllChannels
			} else {
				ScanMethod::Fast
			})
		}
		WifiCommand::Protocols(choice) => {
			let protocols = match choice {
				ProtocolChoice::B => Protocol::B.into(),
				ProtocolChoice::Bg => Protocol::B | Protocol::G,
				ProtocolChoice::Bgn => Protocol::B | Protocol::G | Protocol::N,
				ProtocolChoice::Bgnax => Protocol::B | Protocol::G | Protocol::N | Protocol::AX,
			};
			updated = updated.with_protocols(Protocols::default().with_2_4(protocols));
		}
		WifiCommand::MixedAuthentication(mixed) => {
			let password = match station(&state.saved).map(|station| station.authentication()) {
				Some(
					AuthenticationMethodConfig::Wpa2Personal(password)
					| AuthenticationMethodConfig::WpaWpa2Personal(password)
					| AuthenticationMethodConfig::Wpa(password),
				) => password,
				_ => {
					warn!("diag: no saved WPA password");
					return;
				}
			};
			updated = updated.with_authentication(if mixed {
				AuthenticationMethodConfig::WpaWpa2Personal(password)
			} else {
				AuthenticationMethodConfig::Wpa2Personal(password)
			});
		}
		_ => return,
	}
	let mut config = state.current.clone();
	match &mut config {
		Config::Station(station) | Config::AccessPointStation(station, _) => *station = updated,
		_ => return,
	}
	apply_config(controller, state, config).await;
}

async fn apply_config(controller: &mut WifiController<'static>, state: &mut State, config: Config) {
	disconnect(controller).await;
	match controller.set_config(&config) {
		Ok(()) => {
			state.current = config;
			info!("diag: station override applied");
		}
		Err(error) => {
			warn!("diag: setting station configuration failed: {error:?}");
			if let Err(error) = controller.set_config(&state.current) {
				warn!("diag: restoring station configuration failed: {error:?}");
			}
		}
	}
	status(controller, state);
}

async fn reset(controller: &mut WifiController<'static>, state: &mut State) {
	apply_config(controller, state, state.saved.clone()).await;
	match controller.set_max_tx_power(20) {
		Ok(()) => state.requested_power = 20,
		Err(error) => warn!("diag: restoring TX power failed: {error:?}"),
	}
	match controller.set_power_saving(PowerSaveMode::None) {
		Ok(()) => state.power_save = PowerSaveMode::None,
		Err(error) => warn!("diag: restoring power saving failed: {error:?}"),
	}
	if let Some(widths) = state.bandwidths {
		if let Err(error) = controller.set_bandwidths(widths) {
			warn!("diag: restoring bandwidth failed: {error:?}");
		}
	}
	state.sniffer = None;
	state.enabled = state.saved_enabled;
	info!("diag: WiFi reset finished");
	status(controller, state);
}

fn sniff(controller: &WifiController<'static>, state: &mut State, enabled: bool) {
	if !enabled {
		state.sniffer = None;
		info!("diag: management capture disabled");
		return;
	}
	if state.sniffer.is_some() {
		return;
	}
	packets::set_station(state.mac);
	let mut sniffer = controller.sniffer();
	sniffer.set_receive_cb(packets::receive);
	match sniffer.set_promiscuous_mode(true) {
		Ok(()) => {
			state.sniffer = Some(sniffer);
			info!("diag: management RX capture enabled for this station");
		}
		Err(error) => warn!("diag: enabling management capture failed: {error:?}"),
	}
}
