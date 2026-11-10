//! Carries a call's voice between the overlay and the audio task, and tells
//! the audio task whether to ring.

use esp_ygg_app::Overlay;
use esp_ygg_app::voice::iax2::CallEvent;
use log::{info, warn};
use yggdrasil_espnow::Instant;

use crate::audio;

#[derive(Default)]
pub struct CallLink {
	ringing: bool,
	/// Voice frames sent and received in the current call
	sent: u32,
	received: u32,
	/// Whether dropped voice was reported, once per call
	overflowed: bool,
}

impl CallLink {
	pub fn handle_event(&mut self, event: &CallEvent) {
		match event {
			CallEvent::Incoming { .. } => self.ringing = true,
			CallEvent::Answered => self.ringing = false,
			CallEvent::Ended(reason) => {
				let (sent, received) = (self.sent, self.received);
				*self = CallLink::default();
				info!("Call ended ({reason}): {sent} voice frames sent, {received} received");
			}
			CallEvent::Ringing => {}
		}
	}

	/// Sends a frame from the microphone.
	pub fn send(&mut self, overlay: &mut Overlay, frame: &[u8], now: Instant) {
		overlay.send_voice(frame, now);
		self.sent += 1;
	}

	/// Passes voice that arrived on to the speaker.
	pub fn deliver(&mut self, overlay: &mut Overlay) {
		while let Some(voice) = overlay.poll_voice() {
			self.received += 1;
			if audio::VOICE_IN.try_send(voice).is_err() && !self.overflowed {
				self.overflowed = true;
				warn!("Voice arrived faster than it played, so some was dropped");
			}
		}
		audio::set_call(self.ringing, overlay.call_codec());
	}
}
