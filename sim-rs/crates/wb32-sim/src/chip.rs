//! The memory map (../../sim/wb32fq95.repl):
//!   0x0000_0000  flash alias: booting from flash maps it at 0, which is
//!                where the core fetches the initial SP and reset vector
//!   0x0800_0000  flash, 256 KB: the chip is a WB32FQ95xC, as the DFU tool
//!                reports
//!   0x1FFF_E000  boot ROM (4 KB) and the information blocks (0x1FFF_F000)
//!   0x2000_0000  SRAM, 36 KB on the die
//! Flash and ROM are read-only to the core; tests and loaders write them
//! through `flash_mut`/`rom_mut`.

use crate::diag::Diag;
use cortex_m3_emu::{Bus, Memory, Size};

pub const FLASH_BASE: u32 = 0x0800_0000;
pub const FLASH_SIZE: usize = 0x4_0000;
pub const ROM_BASE: u32 = 0x1FFF_E000;
pub const ROM_SIZE: usize = 0x2000;
pub const SRAM_BASE: u32 = 0x2000_0000;
pub const SRAM_SIZE: usize = 0x9000;

pub struct Wb32 {
    flash: Memory,
    rom: Memory,
    sram: Memory,
    diags: Vec<Diag>,
}

/// Which memory an address falls in, and the offset inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Flash(u32),
    Rom(u32),
    Sram(u32),
}

impl Default for Wb32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Wb32 {
    pub fn new() -> Self {
        Wb32 {
            flash: Memory::new(FLASH_SIZE),
            rom: Memory::new(ROM_SIZE),
            sram: Memory::new(SRAM_SIZE),
            diags: Vec::new(),
        }
    }

    /// The memory holding the whole access, if one does.
    pub fn region(&self, addr: u32, size: Size) -> Option<Region> {
        let inside = |m: &Memory, base: u32| {
            let off = addr.wrapping_sub(base);
            (off < m.len() && m.contains(off, size)).then_some(off)
        };
        if let Some(off) = inside(&self.flash, 0) {
            return Some(Region::Flash(off));
        }
        if let Some(off) = inside(&self.flash, FLASH_BASE) {
            return Some(Region::Flash(off));
        }
        if let Some(off) = inside(&self.rom, ROM_BASE) {
            return Some(Region::Rom(off));
        }
        inside(&self.sram, SRAM_BASE).map(Region::Sram)
    }

    pub fn flash(&self) -> &Memory {
        &self.flash
    }

    pub fn flash_mut(&mut self) -> &mut Memory {
        &mut self.flash
    }

    pub fn rom(&self) -> &Memory {
        &self.rom
    }

    pub fn rom_mut(&mut self) -> &mut Memory {
        &mut self.rom
    }

    pub fn sram(&self) -> &Memory {
        &self.sram
    }

    pub fn sram_mut(&mut self) -> &mut Memory {
        &mut self.sram
    }

    /// Everything reported since the last call.
    pub fn take_diags(&mut self) -> Vec<Diag> {
        std::mem::take(&mut self.diags)
    }

    pub fn diags(&self) -> &[Diag] {
        &self.diags
    }
}

impl Bus for Wb32 {
    fn read(&mut self, addr: u32, size: Size) -> u32 {
        match self.region(addr, size) {
            Some(Region::Flash(o)) => self.flash.read(o, size),
            Some(Region::Rom(o)) => self.rom.read(o, size),
            Some(Region::Sram(o)) => self.sram.read(o, size),
            None => {
                self.diags.push(Diag::Unmapped {
                    addr,
                    size,
                    write: None,
                });
                0
            }
        }
    }

    fn write(&mut self, addr: u32, size: Size, value: u32) {
        let value = value & size.mask();
        match self.region(addr, size) {
            Some(Region::Sram(o)) => self.sram.write(o, size, value),
            Some(Region::Flash(_) | Region::Rom(_)) => {
                self.diags.push(Diag::ReadOnly { addr, size, value })
            }
            None => self.diags.push(Diag::Unmapped {
                addr,
                size,
                write: Some(value),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_is_aliased_at_zero() {
        let mut chip = Wb32::new();
        chip.flash_mut().write(0x100, Size::Word, 0xDEAD_BEEF);
        assert_eq!(chip.read(0x100, Size::Word), 0xDEAD_BEEF);
        assert_eq!(chip.read(FLASH_BASE + 0x100, Size::Word), 0xDEAD_BEEF);
        assert!(chip.diags().is_empty());
    }

    #[test]
    fn sram_is_read_write_flash_and_rom_are_not() {
        let mut chip = Wb32::new();
        chip.write(SRAM_BASE + 0x8FFC, Size::Word, 0x1234_5678);
        assert_eq!(chip.read(SRAM_BASE + 0x8FFC, Size::Word), 0x1234_5678);
        chip.write(FLASH_BASE, Size::Byte, 0x1FF);
        chip.write(ROM_BASE + 0x1000, Size::Half, 1);
        assert_eq!(chip.read(FLASH_BASE, Size::Byte), 0);
        assert_eq!(
            chip.take_diags(),
            [
                Diag::ReadOnly {
                    addr: FLASH_BASE,
                    size: Size::Byte,
                    value: 0xFF
                },
                Diag::ReadOnly {
                    addr: ROM_BASE + 0x1000,
                    size: Size::Half,
                    value: 1
                },
            ]
        );
    }

    #[test]
    fn edges_and_holes_are_unmapped() {
        let mut chip = Wb32::new();
        // The last word of SRAM is in; one that runs past the end is not.
        assert_eq!(
            chip.region(SRAM_BASE + 0x8FFC, Size::Word),
            Some(Region::Sram(0x8FFC))
        );
        assert_eq!(chip.region(SRAM_BASE + 0x8FFE, Size::Word), None);
        assert_eq!(
            chip.region(FLASH_BASE + FLASH_SIZE as u32, Size::Byte),
            None
        );
        assert_eq!(chip.region(ROM_BASE - 1, Size::Byte), None);
        assert_eq!(chip.read(0x4000_0000, Size::Word), 0);
        chip.write(0x6000_0000, Size::Word, 7);
        assert_eq!(
            chip.take_diags(),
            [
                Diag::Unmapped {
                    addr: 0x4000_0000,
                    size: Size::Word,
                    write: None
                },
                Diag::Unmapped {
                    addr: 0x6000_0000,
                    size: Size::Word,
                    write: Some(7)
                },
            ]
        );
    }
}
