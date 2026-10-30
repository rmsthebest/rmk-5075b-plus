//! What the chip model reports instead of faulting: accesses a real chip
//! wouldn't make sense of. Tests fail on any of these unless they say
//! otherwise.

use cortex_m3_emu::Size;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diag {
    /// Nothing is mapped there. Reads gave 0; writes were dropped.
    Unmapped {
        addr: u32,
        size: Size,
        write: Option<u32>,
    },
    /// A store to flash or the ROM, which only the flash controller may
    /// change. The write was dropped.
    ReadOnly { addr: u32, size: Size, value: u32 },
    /// An ANCTL write while PWR's ANAKEY1/2 didn't unlock it. Dropped.
    Locked {
        block: &'static str,
        off: u32,
        value: u32,
    },
    /// A MAINCLK switch (RCC_MAINCLKUEN) to a source that isn't stable:
    /// MAINCLK stays where it was (RM 7.2.9).
    ClockNotReady { src: u32 },
    /// An access to a peripheral RCC gives no clock (or holds in reset):
    /// reads gave 0, writes were dropped (RM 7.3.12-7.3.15).
    ClockOff {
        block: &'static str,
        addr: u32,
        write: Option<u32>,
    },
    /// A fetch or read from the flash macro (main flash, or the ROM in its
    /// system memory) while the FMC was erasing or programming it. The
    /// sim gave the old contents; the chip can't (RM 26.1).
    FlashBusy { addr: u32 },
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Diag::Unmapped {
                addr,
                size,
                write: None,
            } => {
                write!(f, "unmapped read{} at 0x{addr:08x}", size.bytes() * 8)
            }
            Diag::Unmapped {
                addr,
                size,
                write: Some(v),
            } => {
                write!(
                    f,
                    "unmapped write{} at 0x{addr:08x} = 0x{v:x}",
                    size.bytes() * 8
                )
            }
            Diag::Locked { block, off, value } => {
                write!(
                    f,
                    "LOCKED write dropped: {block}+0x{off:03x} = 0x{value:08x}"
                )
            }
            Diag::FlashBusy { addr } => {
                write!(f, "flash read at 0x{addr:08x} while the FMC was busy")
            }
            Diag::ClockNotReady { src } => {
                let name = ["MHSI", "FHSI", "PLL", "HSE"][src as usize & 3];
                write!(f, "MAINCLK switch to {name} before it was stable: ignored")
            }
            Diag::ClockOff {
                block,
                addr,
                write: None,
            } => write!(f, "read of {block} at 0x{addr:08x} with its clock off: 0"),
            Diag::ClockOff {
                block,
                addr,
                write: Some(v),
            } => write!(
                f,
                "write to {block} at 0x{addr:08x} = 0x{v:x} with its clock off: dropped"
            ),
            Diag::ReadOnly { addr, size, value } => {
                write!(
                    f,
                    "write{} to read-only 0x{addr:08x} = 0x{value:x}",
                    size.bytes() * 8
                )
            }
        }
    }
}
