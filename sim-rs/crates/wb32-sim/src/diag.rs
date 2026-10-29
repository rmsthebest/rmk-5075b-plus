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
