//! The memory map (../../sim/wb32fq95.repl):
//!   0x0000_0000  flash alias: booting from flash maps it at 0, which is
//!                where the core fetches the initial SP and reset vector
//!   0x0800_0000  flash, 256 KB: the chip is a WB32FQ95xC, as the DFU tool
//!                reports
//!   0x1FFF_E000  boot ROM (4 KB) and the information blocks (0x1FFF_F000)
//!   0x2000_0000  SRAM, 36 KB on the die
//!   0x4000_0000  GPIO, AFIO, EXTI (gpio.rs)    0x4000_2000  TIM2 (tim2.rs)
//!   0x4001_0000  PWR, ANCTL, IWDG, RCC (sysctl.rs)
//!   0x4001_5400  flash cache controller: remembers writes, as cache.py
//! Flash and ROM are read-only to the core; tests and loaders write them
//! through `flash_mut`/`rom_mut`.
//!
//! The peripherals take word accesses; a narrower one reads its bytes out
//! of the word, and writes them into it (read, modify, write).

use crate::diag::Diag;
use crate::gpio::{Gpio, Matrix};
use crate::sysctl::Sysctl;
use crate::tim2::{self, Tim2};
use cortex_m3_emu::{Bus, IrqLines, Memory, Size};
use std::collections::BTreeMap;
use std::fmt;

pub const FLASH_BASE: u32 = 0x0800_0000;
pub const FLASH_SIZE: usize = 0x4_0000;
pub const ROM_BASE: u32 = 0x1FFF_E000;
pub const ROM_SIZE: usize = 0x2000;
pub const SRAM_BASE: u32 = 0x2000_0000;
pub const SRAM_SIZE: usize = 0x9000;

pub const GPIO_BASE: u32 = 0x4000_0000;
pub const TIM2_BASE: u32 = 0x4000_2000;
pub const SYSCTL_BASE: u32 = 0x4001_0000;
pub const CACHE_BASE: u32 = 0x4001_5400;

pub struct Wb32 {
    flash: Memory,
    rom: Memory,
    sram: Memory,
    diags: Vec<Diag>,
    pub gpio: Gpio,
    pub sysctl: Sysctl,
    pub tim2: Tim2,
    cache: BTreeMap<u32, u32>,
    /// The time `advance` last brought the chip to.
    now: u64,
    /// Register writes, in order, while `Some`: what the Renode models log
    /// as "MMIO W" (../../sim/trace.sh), plus TIM2's.
    pub writes: Option<Vec<Write>>,
}

/// A register write: "BLOCK+0xOFF = 0xVALUE", as the Renode models log it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Write {
    pub block: &'static str,
    pub off: u32,
    pub value: u32,
}

impl fmt::Display for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+0x{:03x} = 0x{:08x}", self.block, self.off, self.value)
    }
}

/// Which peripheral an address is in, and the offset inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Periph {
    Gpio(u32),
    Tim2(u32),
    Sysctl(u32),
    Cache(u32),
}

fn periph(addr: u32) -> Option<Periph> {
    let inside = |base: u32, size: u32| addr.checked_sub(base).filter(|&o| o < size);
    if let Some(o) = inside(GPIO_BASE, 0x2000) {
        Some(Periph::Gpio(o))
    } else if let Some(o) = inside(TIM2_BASE, 0x400) {
        Some(Periph::Tim2(o))
    } else if let Some(o) = inside(SYSCTL_BASE, 0x1000) {
        Some(Periph::Sysctl(o))
    } else {
        inside(CACHE_BASE, 0x400).map(Periph::Cache)
    }
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
            gpio: Gpio::default(),
            sysctl: Sysctl::default(),
            tim2: Tim2::default(),
            cache: BTreeMap::new(),
            now: 0,
            writes: None,
        }
    }

    /// A chip with a key matrix on its pins.
    pub fn with_matrix(matrix: Matrix) -> Self {
        let mut chip = Wb32::new();
        chip.gpio = Gpio::new(matrix);
        chip
    }

    /// A system reset: the peripherals go back to their reset values;
    /// memory, and what's outside the chip, stays.
    pub fn reset(&mut self) {
        self.gpio.reset();
        self.sysctl.reset();
        self.tim2 = Tim2::default();
        self.tim2.sync(self.now);
        self.cache.clear();
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    fn periph_read(&mut self, p: Periph) -> u32 {
        match p {
            Periph::Gpio(o) => self.gpio.read(o),
            Periph::Tim2(o) => self.tim2.read(o, self.now),
            Periph::Sysctl(o) => self.sysctl.read(o),
            Periph::Cache(o) => self.cache.get(&o).copied().unwrap_or(0),
        }
    }

    fn periph_write(&mut self, p: Periph, value: u32) {
        let writes = &mut self.writes;
        let mut log = |w: Write| {
            if let Some(ws) = writes {
                ws.push(w);
            }
        };
        match p {
            Periph::Gpio(o) => self.gpio.write(o, value, &mut log),
            Periph::Tim2(o) => {
                log(Write { block: "TIM2", off: o, value });
                self.tim2.write(o, value, self.now)
            }
            Periph::Sysctl(o) => {
                if !self.sysctl.write(o, value, &mut log) {
                    let (block, off) = Sysctl::block(o);
                    self.diags.push(Diag::Locked { block, off, value });
                }
            }
            Periph::Cache(o) => {
                log(Write { block: "CACHE", off: o, value });
                self.cache.insert(o, value);
            }
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
            None if let Some(p) = periph(addr & !3) => {
                let shift = (addr & 3) * 8;
                (self.periph_read(p) >> shift) & size.mask()
            }
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
            None if let Some(p) = periph(addr & !3) => {
                let value = if size == Size::Word {
                    value
                } else {
                    let shift = (addr & 3) * 8;
                    let old = self.periph_read(p);
                    (old & !(size.mask() << shift)) | value << shift
                };
                self.periph_write(p, value)
            }
            None => self.diags.push(Diag::Unmapped {
                addr,
                size,
                write: Some(value),
            }),
        }
    }

    fn fetch(&mut self, addr: u32) -> Option<u16> {
        match self.region(addr, Size::Half)? {
            Region::Flash(o) => Some(self.flash.read(o, Size::Half) as u16),
            Region::Rom(o) => Some(self.rom.read(o, Size::Half) as u16),
            Region::Sram(o) => Some(self.sram.read(o, Size::Half) as u16),
        }
    }

    fn advance(&mut self, now: u64) {
        self.now = now;
        if self.tim2.next_event() <= now {
            self.tim2.sync(now);
        }
    }

    fn next_event(&self) -> u64 {
        self.tim2.next_event()
    }

    fn irq_lines(&mut self) -> IrqLines {
        let level = self.gpio.irq_level() | (self.tim2.irq() as u64) << tim2::IRQ;
        IrqLines { level, raised: 0 }
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
        assert_eq!(chip.read(0x4000_5000, Size::Word), 0);
        chip.write(0x6000_0000, Size::Word, 7);
        assert_eq!(
            chip.take_diags(),
            [
                Diag::Unmapped {
                    addr: 0x4000_5000,
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
