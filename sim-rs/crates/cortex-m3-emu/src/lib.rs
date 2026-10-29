//! A Cortex-M3 (ARMv7-M, Thumb-2) interpreter.
//!
//! The core owns what's inside the processor: registers, NVIC, SysTick, SCB
//! and DWT (the private peripheral bus at 0xE000_0000). Everything else goes
//! through a [`Bus`] the chip model implements, so the chip decides its own
//! memory map and how peripherals see each other.

pub mod bus;
pub mod decode;
pub mod disasm;

pub use bus::{Bus, Memory, Size};
pub use decode::{Insn, ItState, Op, decode};
pub use disasm::disasm;
