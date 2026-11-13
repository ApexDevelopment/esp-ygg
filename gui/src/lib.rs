//! The graphical interface for boards with a larger touch screen, made with
//! Slint and its Material 3 components.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(feature = "console")]
mod console;
mod front;
#[cfg(feature = "gopher")]
mod gopher;
#[cfg(feature = "irc")]
mod irc;
#[cfg(feature = "gopher")]
mod page;
mod ping;

pub use front::Gui;
pub use slint;

slint::include_modules!();
