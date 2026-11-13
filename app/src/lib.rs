//! What an esp-ygg board offers on top of being a Yggdrasil node: a TCP/IP
//! stack on its Yggdrasil address with a few services, and the user
//! interface. Nothing here touches hardware, so it all runs on a desktop too.
#![no_std]

extern crate alloc;

#[cfg(any(feature = "gopher", feature = "gemini"))]
mod authority;
pub mod device;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "gopher")]
pub mod gopher;
pub mod hotspot;
pub mod http;
#[cfg(feature = "irc")]
pub mod irc;
mod overlay;
pub mod settings;
#[cfg(feature = "share")]
pub mod share;
#[cfg(feature = "ntp")]
pub mod sntp;
pub mod time;
#[cfg(feature = "gemini")]
pub mod tls;
#[cfg(feature = "ui")]
pub mod ui;
#[cfg(feature = "voice")]
pub mod voice;

pub use device::MTU;
pub use http::BoardInfo;
#[cfg(feature = "console")]
pub use overlay::CONSOLE_PORT;
#[cfg(any(feature = "http", feature = "gopher"))]
pub use overlay::HTTP_PORT;
#[cfg(any(feature = "irc", feature = "gemini", feature = "share"))]
pub use overlay::Stream;
pub use overlay::{Event, Overlay, PING_TIMEOUT, SendError};
pub use settings::Settings;
