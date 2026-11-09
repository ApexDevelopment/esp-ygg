//! Lets smoltcp send and receive IPv6 packets through queues, which the
//! overlay fills from and drains into the Yggdrasil node.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv6Address};
use yggdrasil_espnow::yggdrasil_proto::address::Address;

/// The largest IPv6 packet the stack sends, the IPv6 minimum.
pub const MTU: usize = 1280;

/// Packets waiting to enter or leave the stack.
#[derive(Default)]
pub struct Queues {
	pub received: VecDeque<Vec<u8>>,
	pub outgoing: VecDeque<Vec<u8>>,
}

/// A smoltcp interface for this node's Yggdrasil address. The `/7` prefix
/// covers every Yggdrasil address and subnet, so no routes are needed.
pub fn interface(
	address: &Address,
	device: &mut Queues,
	random_seed: u64,
	now: Instant,
) -> Interface {
	let mut config = Config::new(HardwareAddress::Ip);
	config.random_seed = random_seed;
	let mut interface = Interface::new(config, device, now);
	let address = Ipv6Address::from(address.0);
	interface.update_ip_addrs(|addresses| {
		addresses
			.push(IpCidr::new(address.into(), 7))
			.expect("room for one address");
	});
	interface
}

pub struct RxToken(Vec<u8>);

pub struct TxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::RxToken for RxToken {
	fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
		f(&self.0)
	}
}

impl phy::TxToken for TxToken<'_> {
	fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
		let mut packet = vec![0u8; len];
		let result = f(&mut packet);
		self.0.push_back(packet);
		result
	}
}

impl Device for Queues {
	type RxToken<'a> = RxToken;
	type TxToken<'a> = TxToken<'a>;

	fn receive(&mut self, _now: Instant) -> Option<(RxToken, TxToken<'_>)> {
		let packet = self.received.pop_front()?;
		Some((RxToken(packet), TxToken(&mut self.outgoing)))
	}

	fn transmit(&mut self, _now: Instant) -> Option<TxToken<'_>> {
		Some(TxToken(&mut self.outgoing))
	}

	fn capabilities(&self) -> DeviceCapabilities {
		let mut capabilities = DeviceCapabilities::default();
		capabilities.medium = Medium::Ip;
		capabilities.max_transmission_unit = MTU;
		capabilities
	}
}
