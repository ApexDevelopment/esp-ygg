//! The hotspot's access point: frames move between it and the main loop
//! through channels, where the overlay routes them.

use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::Poll;

use embassy_net_driver::{Driver, RxToken, TxToken};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Interface};

pub static FROM_HOTSPOT: Channel<CriticalSectionRawMutex, Vec<u8>, 8> = Channel::new();
pub static TO_HOTSPOT: Channel<CriticalSectionRawMutex, Vec<u8>, 16> = Channel::new();

pub fn config(name: &str, password: &str, channel: u8) -> Option<AccessPointConfig> {
	let authentication = if password.is_empty() {
		AuthenticationMethodConfig::Open
	} else {
		AuthenticationMethodConfig::Wpa2Personal(password.try_into().ok()?)
	};
	Some(
		AccessPointConfig::default()
			.with_ssid(name.try_into().ok()?)
			.with_channel(channel)
			.with_authentication(authentication),
	)
}

enum Next {
	Received(Vec<u8>),
	Send(Vec<u8>),
}

#[embassy_executor::task]
pub async fn link(mut access_point: Interface) {
	loop {
		let next = poll_fn(|cx| {
			if let Some((frame, _)) = Driver::receive(&mut access_point, cx) {
				return Poll::Ready(Next::Received(frame.consume(|frame| frame.to_vec())));
			}
			TO_HOTSPOT.poll_receive(cx).map(Next::Send)
		})
		.await;
		match next {
			// If the main loop falls behind, frames are dropped like on any
			// busy network.
			Next::Received(frame) => {
				let _ = FROM_HOTSPOT.try_send(frame);
			}
			Next::Send(frame) => {
				poll_fn(|cx| match Driver::transmit(&mut access_point, cx) {
					Some(token) => {
						token.consume(frame.len(), |buffer| buffer.copy_from_slice(&frame));
						Poll::Ready(())
					}
					None => Poll::Pending,
				})
				.await
			}
		}
	}
}
