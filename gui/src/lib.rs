//! The graphical interface for boards with a larger touch screen, made with
//! Slint and its Material 3 components.

#![no_std]

extern crate alloc;

#[cfg(feature = "console")]
mod console;
mod front;
#[cfg(feature = "irc")]
mod irc;
mod ping;

pub use front::Gui;
pub use slint;

slint::include_modules!();
