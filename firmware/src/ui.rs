//! Connects the on-device UI to the screen, the keyboard and the rest of
//! the firmware.

use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant};
use esp_ygg_app::ui::{self, App, Command, Key, Status, Uplink, WifiNetwork};
use esp_ygg_app::{BoardInfo, Event, Settings};
use yggdrasil_espnow::Node;

use crate::cardputer::{KEYS, Screen};
use crate::platform::EspPlatform;
use crate::uplink::{SCAN_RESULTS, ScannedNetwork};

const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(50);

pub enum Input {
	Key(Key),
	Networks(Vec<ScannedNetwork>),
}

pub async fn next_input() -> Input {
	match select(KEYS.receive(), SCAN_RESULTS.receive()).await {
		Either::First(key) => Input::Key(key),
		Either::Second(networks) => Input::Networks(networks),
	}
}

pub struct Ui {
	app: App,
	screen: Screen,
	next_status: Instant,
	next_frame: Instant,
}

impl Ui {
	pub fn new(screen: Screen, settings: Settings, node: &Node<EspPlatform>) -> Self {
		Ui {
			app: App::new(settings, node.now()),
			screen,
			next_status: Instant::now(),
			next_frame: Instant::now(),
		}
	}

	pub fn handle_input(&mut self, input: Input) {
		match input {
			Input::Key(key) => self.app.handle_key(key),
			Input::Networks(networks) => self.app.set_networks(
				networks
					.into_iter()
					.map(|network| WifiNetwork {
						ssid: network.ssid,
						signal: network.signal,
						secured: network.secured,
					})
					.collect(),
			),
		}
	}

	pub fn handle_event(&mut self, event: Event) {
		self.app.handle_event(event);
	}

	pub fn handle_nodeinfo(&mut self, key: yggdrasil_espnow::PublicKey, info: Option<Vec<u8>>) {
		self.app.handle_nodeinfo(key, info);
	}

	/// Refreshes the status shown, runs the UI's timers and redraws when
	/// due. Returns what the UI asked for.
	pub fn update(
		&mut self,
		node: &Node<EspPlatform>,
		board: &BoardInfo,
		uplink: Uplink,
	) -> Vec<Command> {
		let now = Instant::now();
		if now >= self.next_status {
			self.app.set_status(Status::from_node(node, board, uplink));
			self.next_status = now + STATUS_INTERVAL;
		}
		self.app.tick(node.now());
		if now >= self.next_frame {
			let _ = self.screen.draw(|frame| ui::draw(frame, &self.app));
			self.next_frame = now + FRAME_INTERVAL;
		}
		core::iter::from_fn(|| self.app.poll_command()).collect()
	}

	/// When [`Ui::update`] should next run.
	pub fn wake_at(&self) -> Instant {
		self.next_status.min(self.next_frame)
	}
}
