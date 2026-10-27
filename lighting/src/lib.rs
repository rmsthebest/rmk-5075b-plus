//! QMK's rgb_matrix for the Akko 5075B Plus: the task state machine, the
//! key hit tracker and every effect the board enables, written to give the
//! same LED colours as QMK's C for the same inputs (`tools/rgbref` checks
//! it against the C, compiled for the host).
//!
//! Where the C reads out of bounds (an effect touching a matrix position
//! without an LED, `NO_LED` = 255), this treats the LED's flags as 0 and
//! ignores the write.

#![no_std]

pub mod board;
pub mod color;
mod effects;
pub mod engine;
pub mod layout;
pub mod math;
pub mod rgblight;

pub use board::{Board, Key, Outcome, Saved};
pub use color::{Hsv, Rgb, hsv_to_rgb};
pub use effects::effect;
pub use engine::{Config, Engine, Hooks, Leds, NoHooks};
