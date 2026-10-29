//! A Cortex-M3 (ARMv7-M, Thumb-2) interpreter.
//!
//! The core owns what's inside the processor: registers, NVIC, SysTick, SCB
//! and DWT (the private peripheral bus at 0xE000_0000). Everything else goes
//! through a [`Bus`] the chip model implements, so the chip decides its own
//! memory map and how peripherals see each other.
//!
//! Time is counted in core ticks of 1/[`TICK_HZ`] s, a rate every clock
//! here divides: 100 MIPS is 24 ticks an instruction, 96 MHz 25 a cycle.

pub mod bus;
pub mod cpu;
pub mod decode;
pub mod disasm;
pub mod exception;
pub mod ppb;

pub use bus::{Bus, IrqLines, Memory, Size};
pub use cpu::{Cpu, Sleep};
pub use decode::{Insn, ItState, Op, decode};
pub use disasm::disasm;
pub use exception::{Exc, Fault};

use std::fmt;

pub const TICK_HZ: u64 = 2_400_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Ticks per instruction while awake.
    pub insn_ticks: u64,
    /// Ticks per SysTick count (both clock sources, as in Renode).
    pub systick_ticks: u64,
    /// Ticks per DWT CYCCNT count.
    pub dwt_ticks: u64,
    /// The implemented priority bits (__NVIC_PRIO_BITS 4: 0xF0).
    pub prio_mask: u8,
}

impl Default for Config {
    /// Renode's Cortex-M timing, so the two simulators agree: 100 MIPS
    /// (its default PerformanceInMips), SysTick and CYCCNT at 96 MHz (the
    /// .repl's systickFrequency and DWT frequency).
    fn default() -> Self {
        Config {
            insn_ticks: TICK_HZ / 100_000_000,
            systick_ticks: TICK_HZ / 96_000_000,
            dwt_ticks: TICK_HZ / 96_000_000,
            prio_mask: 0xF0,
        }
    }
}

/// What the core reports instead of stopping the simulation: each is a
/// firmware (or simulator) bug for a test to fail on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreDiag {
    /// An encoding the ARM ARM calls UNPREDICTABLE was executed.
    Unpredictable {
        pc: u32,
        insn: Insn,
    },
    Fault {
        kind: Fault,
        pc: u32,
    },
    /// A fault with no handler able to take it: the core stopped.
    Lockup {
        pc: u32,
    },
    /// BKPT with no debugger: escalated to HardFault.
    Breakpoint {
        pc: u32,
    },
    /// The PPB outside what's modelled, or used in a way that isn't.
    Ppb {
        addr: u32,
        write: Option<u32>,
        note: &'static str,
    },
}

impl fmt::Display for CoreDiag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreDiag::Unpredictable { pc, insn } => {
                write!(
                    f,
                    "UNPREDICTABLE at 0x{pc:08x}: {}",
                    disasm(insn, *pc, None)
                )
            }
            CoreDiag::Fault { kind, pc } => write!(f, "{kind:?} fault at 0x{pc:08x}"),
            CoreDiag::Lockup { pc } => write!(f, "lockup at 0x{pc:08x}"),
            CoreDiag::Breakpoint { pc } => write!(f, "BKPT at 0x{pc:08x}"),
            CoreDiag::Ppb {
                addr,
                write: None,
                note,
            } => write!(f, "PPB read 0x{addr:08x}: {note}"),
            CoreDiag::Ppb {
                addr,
                write: Some(v),
                note,
            } => {
                write!(f, "PPB write 0x{addr:08x} = 0x{v:x}: {note}")
            }
        }
    }
}
