//! The memory map (../../sim/wb32fq95.repl):
//!   0x0000_0000  flash alias: booting from flash maps it at 0, which is
//!                where the core fetches the initial SP and reset vector
//!   0x0800_0000  flash, 256 KB: the chip is a WB32FQ95xC, as the DFU tool
//!                reports
//!   0x1FFF_E000  boot ROM (4 KB) and the information blocks (0x1FFF_F000)
//!   0x2000_0000  SRAM, 36 KB on the die
//!   0x4000_0000  GPIO, AFIO, EXTI (gpio.rs)    0x4000_2000  TIM2 (tim2.rs)
//!   0x4000_3000  QSPI, 0x4000_9000 SPIM2 (ssi.rs)
//!   0x4000_8400  UART3 (uart.rs)
//!   0x4001_0000  PWR, ANCTL, IWDG, RCC (sysctl.rs)
//!   0x4001_4000  USB device controller, CRC, SFM (usb.rs)
//!   0x4001_5400  flash cache controller: remembers writes, as cache.py
//!   0x4001_6400  SYS, 0x4001_7800 FMC (fmc.rs)
//! Flash and ROM are read-only to the core; tests and loaders write them
//! through `flash_mut`/`rom_mut`.
//!
//! The peripherals take word accesses; a narrower one reads its bytes out
//! of the word, and writes them into it (read, modify, write).

use crate::diag::Diag;
use crate::fmc::{self, Fmc};
use crate::gpio::{Gpio, Matrix};
use crate::ssi::{QSPI_IRQ, SPIM2_IRQ, Ssi};
use crate::sysctl::Sysctl;
use crate::tim2::{self, Tim2};
use crate::uart::{UART3_IRQ, Uart};
use crate::usb::{self, Musb};
use cortex_m3_emu::{Bus, IrqLines, Memory, Size};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;

pub const FLASH_BASE: u32 = 0x0800_0000;
pub const FLASH_SIZE: usize = 0x4_0000;
pub const ROM_BASE: u32 = 0x1FFF_E000;
pub const ROM_SIZE: usize = 0x2000;
pub const SRAM_BASE: u32 = 0x2000_0000;
pub const SRAM_SIZE: usize = 0x9000;

pub const GPIO_BASE: u32 = 0x4000_0000;
pub const TIM2_BASE: u32 = 0x4000_2000;
pub const QSPI_BASE: u32 = 0x4000_3000;
pub const UART3_BASE: u32 = 0x4000_8400;
pub const SPIM2_BASE: u32 = 0x4000_9000;
pub const SYSCTL_BASE: u32 = 0x4001_0000;
pub const CACHE_BASE: u32 = 0x4001_5400;
pub const USB_BASE: u32 = 0x4001_4000;
pub const SYS_BASE: u32 = 0x4001_6400;
pub const FMC_BASE: u32 = 0x4001_7800;

pub struct Wb32 {
    flash: Memory,
    rom: Memory,
    sram: Memory,
    diags: Vec<Diag>,
    pub gpio: Gpio,
    pub sysctl: Sysctl,
    pub tim2: Tim2,
    pub qspi: Ssi,
    pub spim2: Ssi,
    pub uart3: Uart,
    cache: BTreeMap<u32, u32>,
    /// Shared with the USB host, which drives its bus side.
    pub usb: Rc<RefCell<Musb>>,
    pub fmc: Fmc,
    /// The time `advance` last brought the chip to.
    now: u64,
    /// The instruction executing (`Bus::executing`).
    pc: u32,
    /// Times an instruction at each of these addresses ran, while `Some`:
    /// Renode's `cpu AddHook` counters (m7_power's handler counts).
    pub pc_hits: Option<HashMap<u32, u64>>,
    /// Register writes, in order, while `Some`: what the Renode models log
    /// as "MMIO W" (../../sim/trace.sh), plus TIM2's.
    pub writes: Option<Vec<Write>>,
    /// Every write to a peripheral (the core's NVIC/SCB/SysTick included)
    /// and every flash write the FMC makes, in order, while `Some`: what
    /// Renode logs with `LogAllPeripheralsAccess` (../../rom/sim/trace.py).
    pub bus_writes: Option<Vec<BusWrite>>,
}

/// A write as Renode's peripheral access log names it: "name+0xoff = 0xv".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusWrite {
    pub periph: &'static str,
    pub off: u32,
    pub value: u32,
    /// The instruction that made it (Renode tags its log lines with one).
    pub pc: u32,
}

impl fmt::Display for BusWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{:#x} = {:#x}", self.periph, self.off, self.value)
    }
}

/// Renode's peripheral names (../../sim/wb32fq95.repl, akko.resc,
/// ../../rom/sim/rom.resc) and their bases.
const RENODE_NAMES: [(&str, u32, u32); 13] = [
    ("nvic", 0xE000_E000, 0x1000),
    ("dwt", 0xE000_1000, 0x1000),
    ("gpio", GPIO_BASE, 0x2000),
    ("tim2", TIM2_BASE, 0x400),
    ("qspi", QSPI_BASE, 0x100),
    ("uart3", UART3_BASE, 0x100),
    ("spim2", SPIM2_BASE, 0x100),
    ("sysctl", SYSCTL_BASE, 0x1000),
    ("usb", USB_BASE, 0x1000),
    ("cache", CACHE_BASE, 0x400),
    ("sys", SYS_BASE, 0x100),
    ("fmc", FMC_BASE, 0x200),
    ("flash", FLASH_BASE, FLASH_SIZE as u32),
];

fn renode_name(addr: u32) -> Option<(&'static str, u32)> {
    RENODE_NAMES
        .iter()
        .find(|(_, base, size)| addr.wrapping_sub(*base) < *size)
        .map(|&(name, base, _)| (name, addr - base))
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
        write!(
            f,
            "{}+0x{:03x} = 0x{:08x}",
            self.block, self.off, self.value
        )
    }
}

/// Which peripheral an address is in, and the offset inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Periph {
    Gpio(u32),
    Tim2(u32),
    Qspi(u32),
    Spim2(u32),
    Uart3(u32),
    Sysctl(u32),
    Cache(u32),
    Usb(u32),
    Sys(u32),
    Fmc(u32),
}

fn periph(addr: u32) -> Option<Periph> {
    let inside = |base: u32, size: u32| addr.checked_sub(base).filter(|&o| o < size);
    if let Some(o) = inside(GPIO_BASE, 0x2000) {
        Some(Periph::Gpio(o))
    } else if let Some(o) = inside(TIM2_BASE, 0x400) {
        Some(Periph::Tim2(o))
    } else if let Some(o) = inside(QSPI_BASE, 0x100) {
        Some(Periph::Qspi(o))
    } else if let Some(o) = inside(SPIM2_BASE, 0x100) {
        Some(Periph::Spim2(o))
    } else if let Some(o) = inside(UART3_BASE, 0x100) {
        Some(Periph::Uart3(o))
    } else if let Some(o) = inside(SYSCTL_BASE, 0x1000) {
        Some(Periph::Sysctl(o))
    } else if let Some(o) = inside(CACHE_BASE, 0x400) {
        Some(Periph::Cache(o))
    } else if let Some(o) = inside(USB_BASE, 0x1000) {
        Some(Periph::Usb(o))
    } else if let Some(o) = inside(SYS_BASE, 0x100) {
        Some(Periph::Sys(o))
    } else {
        inside(FMC_BASE, 0x200).map(Periph::Fmc)
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
            qspi: Ssi::new("QSPI", QSPI_IRQ),
            spim2: Ssi::new("SPIM2", SPIM2_IRQ),
            uart3: Uart::new("UART3", UART3_IRQ),
            cache: BTreeMap::new(),
            usb: Rc::new(RefCell::new(Musb::new())),
            fmc: Fmc::default(),
            now: 0,
            pc: 0,
            pc_hits: None,
            writes: None,
            bus_writes: None,
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
        self.qspi.reset();
        self.spim2.reset();
        self.uart3.reset();
        self.cache.clear();
        *self.usb.borrow_mut() = Musb::new();
        self.fmc = Fmc::default();
    }

    fn log_bus_write(&mut self, addr: u32, value: u32) {
        if let Some(log) = &mut self.bus_writes
            && let Some((periph, off)) = renode_name(addr)
        {
            log.push(BusWrite {
                periph,
                off,
                value,
                pc: self.pc,
            });
        }
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    fn periph_read(&mut self, p: Periph) -> u32 {
        match p {
            Periph::Gpio(o) => self.gpio.read(o),
            Periph::Tim2(o) => self.tim2.read(o, self.now),
            Periph::Qspi(o) => self.qspi.read(o),
            Periph::Spim2(o) => self.spim2.read(o),
            Periph::Uart3(o) => self.uart3.read(o),
            Periph::Sysctl(o) => self.sysctl.read(o),
            Periph::Cache(o) => self.cache.get(&o).copied().unwrap_or(0),
            Periph::Usb(o) => self.usb.borrow_mut().read(o, Size::Word),
            Periph::Sys(o) => fmc::sys_read(o),
            Periph::Fmc(o) => self.fmc.read(o),
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
                log(Write {
                    block: "TIM2",
                    off: o,
                    value,
                });
                self.tim2.write(o, value, self.now)
            }
            Periph::Qspi(o) => self.qspi.write(o, value, &self.gpio, &mut log),
            Periph::Spim2(o) => self.spim2.write(o, value, &self.gpio, &mut log),
            Periph::Uart3(o) => self.uart3.write(o, value, self.now, &self.gpio, &mut log),
            Periph::Sysctl(o) => {
                if !self.sysctl.write(o, value, &mut log) {
                    let (block, off) = Sysctl::block(o);
                    self.diags.push(Diag::Locked { block, off, value });
                }
            }
            Periph::Cache(o) => {
                log(Write {
                    block: "CACHE",
                    off: o,
                    value,
                });
                self.cache.insert(o, value);
            }
            Periph::Usb(o) => {
                if o >= 0x400 {
                    let block = if o < 0xC00 { "CRC" } else { "SFM" };
                    log(Write {
                        block,
                        off: o % 0x400,
                        value,
                    });
                }
                self.usb.borrow_mut().write(o, value);
            }
            Periph::Sys(_) => {}
            Periph::Fmc(o) => {
                if let Some(fill) = self.fmc.write(o, value) {
                    for (i, w) in fill.words.into_iter().enumerate() {
                        let a = fill.addr + 4 * i as u32;
                        match self.region(a, Size::Word) {
                            Some(Region::Flash(f)) => self.flash.write(f, Size::Word, w),
                            Some(Region::Rom(r)) => self.rom.write(r, Size::Word, w),
                            _ => {}
                        }
                        self.log_bus_write(a, w);
                    }
                }
            }
        }
    }

    /// Whether anything answers at `addr`: a memory or a peripheral.
    pub fn mapped(&self, addr: u32, size: Size) -> bool {
        self.region(addr, size).is_some() || periph(addr & !3).is_some()
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
            None if let Some(Periph::Usb(o)) = periph(addr) => self.usb.borrow_mut().read(o, size),
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
            None if let Some(Periph::Usb(o)) = periph(addr) => {
                self.log_bus_write(addr, value);
                self.periph_write(Periph::Usb(o), value)
            }
            None if let Some(p) = periph(addr & !3) => {
                self.log_bus_write(addr, value);
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
        self.uart3.advance(now);
    }

    fn next_event(&self) -> u64 {
        self.tim2.next_event().min(self.uart3.next_event())
    }

    fn executing(&mut self, pc: u32) {
        self.pc = pc;
        if let Some(n) = self.pc_hits.as_mut().and_then(|h| h.get_mut(&pc)) {
            *n += 1;
        }
    }

    fn ppb_write(&mut self, addr: u32, _size: Size, value: u32) {
        self.log_bus_write(addr, value);
    }

    fn irq_lines(&mut self) -> IrqLines {
        let level = self.gpio.irq_level()
            | (self.tim2.irq() as u64) << tim2::IRQ
            | (self.usb.borrow().irq() as u64) << usb::IRQ
            | self.qspi.irq_level()
            | self.spim2.irq_level()
            | self.uart3.irq_level();
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
