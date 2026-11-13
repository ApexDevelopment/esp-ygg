//! The graphical interface for boards with a larger touch screen, made with
//! Slint and its Material 3 components.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(feature = "console")]
mod console;
mod front;
#[cfg(feature = "gemini")]
mod gemini;
#[cfg(feature = "gopher")]
mod gopher;
#[cfg(feature = "irc")]
mod irc;
#[cfg(any(feature = "gopher", feature = "gemini"))]
mod page;
mod ping;
#[cfg(feature = "share")]
mod share;

pub use front::Gui;
pub use slint;

slint::include_modules!();
