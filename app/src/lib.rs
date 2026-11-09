//! What an esp-ygg board offers on top of being a Yggdrasil node: a TCP/IP
//! stack on its Yggdrasil address with a few services, and the user
//! interface. Nothing here touches hardware, so it all runs on a desktop too.
#![no_std]

extern crate alloc;

pub mod device;
pub mod gopher;
pub mod http;
pub mod irc;
mod overlay;
pub mod settings;
pub mod sntp;
pub mod time;
#[cfg(feature = "ui")]
pub mod ui;

pub use device::MTU;
pub use http::BoardInfo;
pub use overlay::{CONSOLE_PORT, Event, HTTP_PORT, Overlay, PING_TIMEOUT, SendError};
pub use settings::Settings;
