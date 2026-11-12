//! The graphical interface for boards with a larger touch screen, made with
//! Slint and its Material 3 components.

#![no_std]

extern crate alloc;

mod front;

pub use front::Gui;
pub use slint;

slint::include_modules!();
