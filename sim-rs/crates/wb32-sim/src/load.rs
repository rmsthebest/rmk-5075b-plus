//! Putting images into memory: raw binaries at an address, and ELF files by
//! their loadable segments' physical addresses (the load address, so .data
//! lands in flash for the startup code to copy, as Renode's LoadELF does).

use crate::chip::{Region, Wb32};
use cortex_m3_emu::Size;
use object::Endianness;
use object::elf::{EM_ARM, FileHeader32, PT_LOAD};
use object::read::elf::{FileHeader, ProgramHeader};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError(pub String);

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoadError {}

impl Wb32 {
    /// Writes `bytes` at `addr`, which must lie wholly in flash, ROM or SRAM.
    pub fn load(&mut self, addr: u32, bytes: &[u8]) -> Result<(), LoadError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let end = addr as u64 + bytes.len() as u64 - 1;
        let first = self.region(addr, Size::Byte);
        let last = u32::try_from(end)
            .ok()
            .and_then(|e| self.region(e, Size::Byte));
        let same = match (first, last) {
            (Some(Region::Flash(a)), Some(Region::Flash(b)))
            | (Some(Region::Rom(a)), Some(Region::Rom(b)))
            | (Some(Region::Sram(a)), Some(Region::Sram(b))) => b - a + 1 == bytes.len() as u32,
            _ => false,
        };
        if !same {
            return Err(LoadError(format!(
                "0x{addr:08x}..=0x{end:08x} isn't inside one memory"
            )));
        }
        let (mem, off) = match first.unwrap() {
            Region::Flash(o) => (self.flash_mut(), o),
            Region::Rom(o) => (self.rom_mut(), o),
            Region::Sram(o) => (self.sram_mut(), o),
        };
        mem.bytes_mut()[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Loads every PT_LOAD segment's file bytes at its physical address.
    /// Returns the ELF entry point.
    pub fn load_elf(&mut self, data: &[u8]) -> Result<u32, LoadError> {
        let err = |e: object::read::Error| LoadError(format!("not a 32-bit ARM ELF: {e}"));
        let hdr = FileHeader32::<Endianness>::parse(data).map_err(err)?;
        let endian = hdr.endian().map_err(err)?;
        if hdr.e_machine(endian) != EM_ARM {
            return Err(LoadError(format!(
                "ELF machine {} isn't ARM",
                hdr.e_machine(endian)
            )));
        }
        for ph in hdr.program_headers(endian, data).map_err(err)? {
            if ph.p_type(endian) != PT_LOAD {
                continue;
            }
            let bytes = ph
                .data(endian, data)
                .map_err(|()| LoadError("segment runs past the end of the file".into()))?;
            self.load(ph.p_paddr(endian), bytes)?;
        }
        Ok(hdr.e_entry(endian))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{FLASH_BASE, SRAM_BASE};

    #[test]
    fn load_checks_bounds() {
        let mut chip = Wb32::new();
        chip.load(FLASH_BASE + 4, &[1, 2, 3]).unwrap();
        assert_eq!(&chip.flash().bytes()[4..7], &[1, 2, 3]);
        assert!(chip.load(SRAM_BASE + 0x8FFF, &[1, 2]).is_err());
        assert!(chip.load(0x4000_0000, &[1]).is_err());
        assert!(chip.load(u32::MAX, &[1, 2]).is_err());
        chip.load(SRAM_BASE + 0x8FFF, &[9]).unwrap();
    }

    #[test]
    fn rejects_non_elf() {
        assert!(Wb32::new().load_elf(b"not an elf").is_err());
    }
}
