//! A Yggdrasil node that peers with nearby nodes over ESP-NOW, and optionally
//! over byte streams such as TCP.
//!
//! [`Node`] does no I/O. The host passes it radio frames and stream bytes,
//! sends what it returns, and calls it again when its timer expires. That keeps
//! it independent of any particular radio driver or executor, and lets it run
//! in simulations on a desktop.
//!
//! Nodes find each other with broadcast beacons, open an
//! [`espnow_reliable::Link`] to each neighbor, and run the normal Yggdrasil
//! link handshake over it. The node answers pings to its address and nodeinfo
//! and debug requests. Other IPv6 packets addressed to it are queued for the
//! application.
#![no_std]

extern crate alloc;

mod espnow;
mod node;
mod stream;

pub use espnow::{BEACON_VERSION, Destination, FRAME_LEN, Mac};
pub use node::{Config, NewError, Node, StreamId};
pub use petrified::types::{Instant, Platform, PrivateKey, PublicKey};
pub use stream::StreamError;
pub use yggdrasil_proto;
