//! The memory map (Renode's sim/wb32fq95.repl):
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
//!
//! A peripheral answers only while RCC gives it its clock (RM 7.3.12-7.3.15:
//! its enable bit, and for APB ones the bus clock and the bus matrix's
//! BMXnEN) and doesn't hold it in reset; otherwise reads give 0, writes are
//! dropped and `Diag::ClockOff` reports it. Setting a reset bit (RM
//! 7.3.21-7.3.23) puts the model back to its reset state. The system
//! control block (PWR, ANCTL, IWDG, RCC) has no gate.
//!
//! Stop mode (RM 5.3.4, table 5-6): a WFI or WFE with SLEEPDEEP set and
//! PWR CR0 PDDS 00. With an EXTI request pending it isn't entered: the
//! core carries on. Stopped, every clock is off (HCLK too: SysTick and
//! CYCCNT stand still; TIM2, the SSIs and UART3 freeze) and only the EXTI
//! lines reach the NVIC. Waking takes `STOP_WAKE` (the MHSI start-up, DS
//! 5.2.4; the regulator's and bandgap's aren't given), then the chip runs
//! from MHSI with every clock configuration at its reset value
//! (`Sysctl::stop_exit`). Standby (PDDS 10) isn't modelled: a plain sleep.

use crate::diag::Diag;
use crate::fmc::{self, Fmc};
use crate::gpio::{Gpio, Matrix};
use crate::power::{self, Core, Meter};
use crate::ssi::{QSPI_FIFO_DEPTH, QSPI_IRQ, SPIM2_FIFO_DEPTH, SPIM2_IRQ, Ssi};
use crate::sysctl::{self, Gate, Refused, Sysctl};
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

/// The trim the chip loads into ANCTL+0x004 at reset (assumed: the RM
/// doesn't list that register; the vendor's stop exit puts this back).
pub const INFO_TRIM: u32 = 0x1FFF_F310;

/// From an EXTI edge in stop to the core running: the MHSI's start-up, at
/// its maximum (DS 5.2.4). A board check.
pub const STOP_WAKE: u64 = 2 * cortex_m3_emu::TICK_HZ / 1_000_000;

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
    /// The FMC op whose busy time a read already broke (`Diag::FlashBusy`).
    flash_busy_seen: u64,
    /// The time `advance` last brought the chip to.
    now: u64,
    /// In stop mode.
    stopped: bool,
    /// In sleep mode (the core's WFI/WFE without a stop).
    asleep: bool,
    /// The supply current drawn so far (`power`).
    meter: Meter,
    /// Stops entered, and stops skipped for a pending EXTI request.
    pub stops: u32,
    pub stops_skipped: u32,
    /// The instruction executing (`Bus::executing`).
    pc: u32,
    /// Times an instruction at each of these addresses ran, while `Some`:
    /// Renode's `cpu AddHook` counters (m7_power's handler counts).
    pub pc_hits: Option<HashMap<u32, u64>>,
    /// Register writes, in order, while `Some`: what the Renode models log
    /// as "MMIO W" (Renode's sim/trace.sh), plus TIM2's.
    pub writes: Option<Vec<Write>>,
    /// Every write to a peripheral (the core's NVIC/SCB/SysTick included)
    /// and every flash write the FMC makes, in order, while `Some`: what
    /// Renode logs with `LogAllPeripheralsAccess` (Renode's rom/sim/trace.py).
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

/// The peripheral names the golden traces use (Renode's sim/wb32fq95.repl,
/// akko.resc and rom/sim/rom.resc) and their bases.
const TRACE_NAMES: [(&str, u32, u32); 13] = [
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

fn trace_name(addr: u32) -> Option<(&'static str, u32)> {
    TRACE_NAMES
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

/// GPIOD's unused space the tests drive the pins through (gpio.rs): not a
/// register, so not gated.
const SIM_CONTROL: u32 = 0xFF0;

impl Periph {
    /// The clock it needs, and its name for the diag; `None`: always on.
    fn gate(self) -> Option<(Gate, &'static str)> {
        Some(match self {
            Periph::Gpio(o) if (SIM_CONTROL..0x1000).contains(&o) => return None,
            Periph::Gpio(o) if o < 0x1000 => {
                let port = o / 0x400;
                (
                    Gate::Apb1(5 + port),
                    ["GPIOA", "GPIOB", "GPIOC", "GPIOD"][port as usize],
                )
            }
            Periph::Gpio(o) if o < 0x1800 => (Gate::Apb1(10), "AFIO"),
            Periph::Gpio(_) => (Gate::Apb1(9), "EXTI"),
            Periph::Tim2(_) => (Gate::Apb1(2), "TIM2"),
            Periph::Qspi(_) => (Gate::Apb1(12), "QSPI"),
            Periph::Uart3(_) => (Gate::Apb2(3), "UART3"),
            Periph::Spim2(_) => (Gate::Apb2(4), "SPIM2"),
            Periph::Usb(o) if o < 0x400 => (Gate::Ahb(1), "USB"),
            Periph::Usb(_) => (Gate::Ahb(8), "CRC/SFM"),
            Periph::Fmc(_) => (Gate::Ahb(3), "FMC"),
            Periph::Cache(_) => (Gate::Ahb(4), "CACHE"),
            Periph::Sys(_) => (Gate::Ahb(5), "SYS"),
            Periph::Sysctl(_) => return None,
        })
    }
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
            qspi: Ssi::new("QSPI", QSPI_IRQ, QSPI_FIFO_DEPTH),
            spim2: Ssi::new("SPIM2", SPIM2_IRQ, SPIM2_FIFO_DEPTH),
            uart3: Uart::new("UART3", UART3_IRQ),
            cache: BTreeMap::new(),
            usb: Rc::new(RefCell::new(Musb::new())),
            fmc: Fmc::default(),
            flash_busy_seen: 0,
            now: 0,
            stopped: false,
            asleep: false,
            meter: Meter::default(),
            stops: 0,
            stops_skipped: 0,
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
        self.meter();
        self.asleep = false;
        self.gpio.reset();
        self.sysctl.reset();
        self.sysctl
            .load_trim(self.rom.read(INFO_TRIM - ROM_BASE, Size::Byte) & 0xF);
        self.tim2 = Tim2::default();
        self.tim2.sync(self.now);
        self.qspi.reset();
        self.spim2.reset();
        self.uart3.reset();
        self.cache.clear();
        *self.usb.borrow_mut() = Musb::new();
        self.fmc = Fmc::default();
        self.stopped = false;
        self.clock_timers();
    }

    /// The charge drawn since power-on (power.rs), up to now.
    pub fn power(&mut self) -> Meter {
        self.meter();
        self.meter
    }

    /// Charges the meter up to now, before anything it depends on changes:
    /// sleeping, waking, the clocks, the oscillators, an FMC op, a reset.
    fn meter(&mut self) {
        let core = if self.stopped {
            Core::Stopped
        } else if self.asleep {
            Core::Asleep
        } else {
            Core::Awake
        };
        let fmc = &self.fmc;
        let state = power::State {
            core,
            hclk: self.sysctl.clocks().hclk,
            osc: self.sysctl.oscillators(),
            flash: (fmc.busy_until > self.meter.at).then_some((
                fmc.busy_from,
                fmc.busy_until,
                fmc.busy_ua,
            )),
        };
        self.meter.charge(self.now, state);
    }

    /// Whether `p` answers now; reports it if not.
    fn clocked(&mut self, p: Periph, addr: u32, write: Option<u32>) -> bool {
        match p.gate() {
            Some((g, block)) if !self.sysctl.clock_on(g) => {
                self.diags.push(Diag::ClockOff { block, addr, write });
                false
            }
            _ => true,
        }
    }

    /// The models that keep time get their bus clock while RCC gives it
    /// to them: TIM2 and QSPI PCLK1, SPIM2 PCLK2.
    fn clock_timers(&mut self) {
        let c = self.sysctl.clocks();
        let stopped = self.stopped;
        let hz = |g| {
            if !stopped && self.sysctl.clock_on(g) {
                1
            } else {
                0
            }
        };
        let (tim2, qspi, spim2, uart3) = (
            hz(Gate::Apb1(2)) * c.pclk1,
            hz(Gate::Apb1(12)) * c.pclk1,
            hz(Gate::Apb2(4)) * c.pclk2,
            hz(Gate::Apb2(3)) * c.pclk2,
        );
        if tim2 != self.tim2.hz() {
            self.tim2.set_clock(tim2, self.now);
        }
        if qspi != self.qspi.hz() {
            self.qspi.set_clock(qspi, self.now);
        }
        if spim2 != self.spim2.hz() {
            self.spim2.set_clock(spim2, self.now);
        }
        if uart3 != self.uart3.hz() {
            self.uart3.set_clock(uart3, self.now, &self.gpio);
        }
    }

    /// RCC reset bits now set (RM 7.3.21-7.3.23): those models go back to
    /// their reset state. GPIO ports, AFIO and EXTI reset one by one; what's
    /// outside the chip stays.
    fn rcc_resets(&mut self) {
        let held = |g| {
            let (reg, bit) = match g {
                Gate::Ahb(b) => (sysctl::AHBRSTR1, b),
                Gate::Apb1(b) => (sysctl::APB1RSTR, b),
                Gate::Apb2(b) => (sysctl::APB2RSTR, b),
            };
            self.sysctl.reg(reg) & 1 << bit != 0
        };
        let (ports, afio, exti) = (
            [5, 6, 7, 8].map(|b| held(Gate::Apb1(b))),
            held(Gate::Apb1(10)),
            held(Gate::Apb1(9)),
        );
        let (tim2, qspi) = (held(Gate::Apb1(2)), held(Gate::Apb1(12)));
        let (uart3, spim2) = (held(Gate::Apb2(3)), held(Gate::Apb2(4)));
        let (usb, fmc, cache) = (held(Gate::Ahb(1)), held(Gate::Ahb(3)), held(Gate::Ahb(4)));
        for (port, r) in ports.into_iter().enumerate() {
            if r {
                self.gpio.reset_port(port as u8);
            }
        }
        if afio {
            self.gpio.reset_afio();
        }
        if exti {
            self.gpio.reset_exti();
        }
        if tim2 {
            let hz = self.tim2.hz();
            self.tim2 = Tim2::default();
            self.tim2.set_clock(hz, self.now);
        }
        if qspi {
            self.qspi.reset();
        }
        if uart3 {
            self.uart3.reset();
        }
        if spim2 {
            self.spim2.reset();
        }
        if usb {
            self.usb.borrow_mut().reset();
        }
        if fmc {
            self.fmc = Fmc::default();
        }
        if cache {
            self.cache.clear();
        }
    }

    fn log_bus_write(&mut self, addr: u32, value: u32) {
        if let Some(log) = &mut self.bus_writes
            && let Some((periph, off)) = trace_name(addr)
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
            Periph::Sysctl(o) => self.sysctl.read(o, self.now),
            Periph::Cache(o) => self.cache.get(&o).copied().unwrap_or(0),
            Periph::Usb(o) => self.usb.borrow_mut().read(o, Size::Word),
            Periph::Sys(o) => fmc::sys_read(o),
            Periph::Fmc(o) => self.fmc.read(o, self.now),
        }
    }

    fn periph_write(&mut self, p: Periph, value: u32) {
        if matches!(p, Periph::Sysctl(_) | Periph::Fmc(_)) {
            self.meter();
        }
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
            Periph::Qspi(o) => self.qspi.write(o, value, self.now, &self.gpio, &mut log),
            Periph::Spim2(o) => self.spim2.write(o, value, self.now, &self.gpio, &mut log),
            Periph::Uart3(o) => self.uart3.write(o, value, self.now, &self.gpio, &mut log),
            Periph::Sysctl(o) => {
                match self.sysctl.write(o, value, self.now, &mut log) {
                    Ok(()) => {}
                    Err(Refused::Locked) => {
                        let (block, off) = Sysctl::block(o);
                        self.diags.push(Diag::Locked { block, off, value });
                    }
                    Err(Refused::NotReady { src }) => self.diags.push(Diag::ClockNotReady { src }),
                }
                if matches!(o, sysctl::AHBRSTR1 | sysctl::APB1RSTR | sysctl::APB2RSTR) {
                    self.rcc_resets();
                }
                self.clock_timers();
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
                if let Some(fill) = self.fmc.write(o, value, self.now) {
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
    /// Flash and the ROM are one macro, unreadable while the FMC works on
    /// it: reported once an op.
    fn check_flash_idle(&mut self, addr: u32, region: Option<Region>) {
        if matches!(region, Some(Region::Flash(_) | Region::Rom(_)))
            && self.fmc.busy(self.now)
            && self.flash_busy_seen != self.fmc.busy_until
        {
            self.flash_busy_seen = self.fmc.busy_until;
            self.diags.push(Diag::FlashBusy { addr });
        }
    }

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
        let region = self.region(addr, size);
        self.check_flash_idle(addr, region);
        match region {
            Some(Region::Flash(o)) => self.flash.read(o, size),
            Some(Region::Rom(o)) => self.rom.read(o, size),
            Some(Region::Sram(o)) => self.sram.read(o, size),
            None if let Some(p) = periph(addr)
                && !self.clocked(p, addr, None) =>
            {
                0
            }
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
            None if let Some(p) = periph(addr)
                && !self.clocked(p, addr, Some(value)) =>
            {
                self.log_bus_write(addr, value)
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
        let region = self.region(addr, Size::Half);
        self.check_flash_idle(addr, region);
        match region? {
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
        self.uart3.advance(now, &self.gpio);
        if let Some(pin) = self.uart3.rx_pin {
            let level = self.uart3.rx_level();
            if self.gpio.driven.insert(pin, level) != Some(level) {
                self.gpio.update();
            }
        }
        if self.qspi.next_event() <= now {
            self.qspi.advance(now, &self.gpio);
        }
        if self.spim2.next_event() <= now {
            self.spim2.advance(now, &self.gpio);
        }
    }

    fn next_event(&self) -> u64 {
        self.tim2
            .next_event()
            .min(self.uart3.next_event())
            .min(self.qspi.next_event())
            .min(self.spim2.next_event())
    }

    /// The clock tree's AHB clock (sysctl.rs); none while stopped.
    fn hclk_hz(&self) -> u64 {
        if self.stopped {
            0
        } else {
            self.sysctl.clocks().hclk
        }
    }

    fn sleep(&mut self, deep: bool) -> bool {
        self.meter();
        if !deep || !self.sysctl.deep_sleep_is_stop() {
            self.asleep = true;
            return true;
        }
        if self.gpio.exti_pending() != 0 {
            self.stops_skipped += 1;
            return false;
        }
        self.stopped = true;
        self.stops += 1;
        self.clock_timers();
        true
    }

    fn woke(&mut self) -> u64 {
        self.meter();
        self.asleep = false;
        if !std::mem::take(&mut self.stopped) {
            return 0;
        }
        self.sysctl.stop_exit(self.now + STOP_WAKE);
        self.clock_timers();
        STOP_WAKE
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
        if self.stopped {
            return IrqLines {
                level: self.gpio.irq_level(),
                raised: 0,
                event: self.gpio.take_event(),
            };
        }
        let level = self.gpio.irq_level()
            | (self.tim2.irq() as u64) << tim2::IRQ
            | (self.usb.borrow().irq() as u64) << usb::IRQ
            | self.qspi.irq_level()
            | self.spim2.irq_level()
            | self.uart3.irq_level();
        IrqLines {
            level,
            raised: 0,
            event: self.gpio.take_event(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_m3_emu::TICK_HZ;

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

    const RCC: u32 = SYSCTL_BASE + 0xC00;
    /// GPIOA's ODR.
    const PA_ODR: u32 = GPIO_BASE + 0x14;

    /// APB1 on and undivided, its bus matrix and `bits` enabled.
    fn apb1_on(chip: &mut Wb32, bits: u32) {
        chip.write(RCC + 0x1C, Size::Word, 0x80);
        chip.write(RCC + 0x48, Size::Word, 1 << 15 | bits);
    }

    #[test]
    fn a_peripheral_answers_only_with_its_clock() {
        let mut chip = Wb32::new();
        // From reset APB1 has no clock: the write is dropped, the read is 0.
        chip.write(PA_ODR, Size::Word, 0x55);
        assert_eq!(chip.read(PA_ODR, Size::Word), 0);
        // GPIOA's enable alone isn't enough without the bus matrix.
        chip.write(RCC + 0x1C, Size::Word, 0x80);
        chip.write(RCC + 0x48, Size::Word, 1 << 5);
        chip.write(PA_ODR, Size::Word, 0x66);
        assert_eq!(
            chip.take_diags(),
            [
                Diag::ClockOff {
                    block: "GPIOA",
                    addr: PA_ODR,
                    write: Some(0x55)
                },
                Diag::ClockOff {
                    block: "GPIOA",
                    addr: PA_ODR,
                    write: None
                },
                Diag::ClockOff {
                    block: "GPIOA",
                    addr: PA_ODR,
                    write: Some(0x66)
                },
            ]
        );
        apb1_on(&mut chip, 1 << 5);
        chip.write(PA_ODR, Size::Word, 0x77);
        assert_eq!(chip.read(PA_ODR, Size::Word), 0x77);
        // GPIOB has its own bit; the sim's control words are always there.
        assert_eq!(chip.read(GPIO_BASE + 0x400 + 0x14, Size::Word), 0);
        assert_eq!(chip.take_diags().len(), 1);
        chip.write(GPIO_BASE + 0xFF8, Size::Word, 1);
        // The system control block has no gate.
        assert_eq!(chip.read(RCC + 0x48, Size::Word), 1 << 15 | 1 << 5);
        assert!(chip.diags().is_empty());
    }

    #[test]
    fn a_reset_bit_holds_the_model_in_reset_then_releases_it() {
        let mut chip = Wb32::new();
        apb1_on(&mut chip, 1 << 5 | 1 << 6);
        chip.write(PA_ODR, Size::Word, 0x77);
        chip.write(GPIO_BASE + 0x400 + 0x14, Size::Word, 0x88);
        chip.write(RCC + 0x90, Size::Word, 1 << 5);
        assert_eq!(chip.read(PA_ODR, Size::Word), 0, "held in reset");
        assert_eq!(chip.take_diags().len(), 1);
        chip.write(RCC + 0x90, Size::Word, 0);
        assert_eq!(chip.read(PA_ODR, Size::Word), 0, "back to its reset value");
        assert_eq!(chip.read(GPIO_BASE + 0x400 + 0x14, Size::Word), 0x88);
        assert!(chip.diags().is_empty());
    }

    #[test]
    fn gpio_registers_come_up_at_the_rms_reset_values() {
        // RM 8.4.1, 8.4.3, 8.4.4, 8.4.11; and the same after an RCC reset.
        let mut chip = Wb32::new();
        apb1_on(&mut chip, 0xF << 5);
        let regs = |chip: &mut Wb32, port: u32| {
            [0x00, 0x08, 0x0C, 0x28].map(|o| chip.read(GPIO_BASE + port * 0x400 + o, Size::Word))
        };
        let a = [0x2A80_0000, 0x0F00_0000, 0x2580_0000, 0xFFFF];
        assert_eq!(regs(&mut chip, 0), a);
        assert_eq!(regs(&mut chip, 1), [0, 0, 0, 0xFFFF]);
        assert_eq!(regs(&mut chip, 3), [0, 0, 0, 0x7]);
        chip.write(GPIO_BASE, Size::Word, 0);
        chip.write(RCC + 0x90, Size::Word, 1 << 5);
        chip.write(RCC + 0x90, Size::Word, 0);
        assert_eq!(regs(&mut chip, 0), a);
        assert!(chip.diags().is_empty());
    }

    #[test]
    fn tim2_counts_pclk1_only_while_enabled() {
        let mut chip = Wb32::new();
        let cnt = TIM2_BASE + 0x24;
        chip.write(RCC + 0x1C, Size::Word, 0x80);
        chip.write(RCC + 0x48, Size::Word, 1 << 15);
        // CEN with TIM2's clock off is dropped, and the timer has no clock.
        chip.write(TIM2_BASE, Size::Word, 1);
        assert_eq!(chip.tim2.hz(), 0);
        apb1_on(&mut chip, 1 << 2);
        chip.write(TIM2_BASE + 0x2C, Size::Word, 0xFFFF);
        chip.write(TIM2_BASE, Size::Word, 1);
        // PCLK1 = MHSI 8 MHz: 8 counts a microsecond.
        assert_eq!(chip.tim2.hz(), 8_000_000);
        chip.advance(TICK_HZ / 1_000_000);
        assert_eq!(chip.read(cnt, Size::Word), 8);
        // Gating APB1's clock (SRCEN) stops it.
        chip.write(RCC + 0x1C, Size::Word, 0);
        assert_eq!(chip.tim2.hz(), 0);
    }

    #[test]
    fn a_stop_freezes_the_clocks_until_an_exti_line_and_wakes_on_mhsi() {
        let mut chip = Wb32::new();
        let (exti, pwr) = (GPIO_BASE + 0x1800, SYSCTL_BASE);
        apb1_on(&mut chip, 1 << 2 | 1 << 9);
        chip.write(TIM2_BASE, Size::Word, 1);
        // EXTI 0 (A0, held high) on its falling edge.
        chip.write(GPIO_BASE + 0xFF4, Size::Word, 1 << 16);
        chip.write(exti + 0x0C, Size::Word, 1);
        chip.write(exti, Size::Word, 1);
        // Not SLEEPDEEP, or a request pending: no stop.
        assert!(chip.sleep(false));
        assert_eq!(chip.hclk_hz(), 8_000_000);
        chip.write(exti + 0x10, Size::Word, 1);
        assert!(!chip.sleep(true));
        assert_eq!((chip.stops, chip.stops_skipped), (0, 1));
        chip.write(exti + 0x14, Size::Word, 1);
        chip.write(exti + 0x10, Size::Word, 0);
        assert!(chip.sleep(true));
        assert_eq!((chip.hclk_hz(), chip.tim2.hz()), (0, 0));
        // Only EXTI reaches the NVIC.
        chip.advance(TICK_HZ);
        assert_eq!(chip.irq_lines().level, 0);
        chip.write(GPIO_BASE + 0xFF4, Size::Word, 0);
        assert_eq!(chip.irq_lines().level, 1 << 6, "EXTI0");
        assert_eq!(chip.woke(), STOP_WAKE);
        let c = chip.sysctl.clocks();
        assert_eq!((c.hclk, c.pclk1), (8_000_000, 0), "MHSI, APB1 off");
        assert_eq!(chip.read(RCC + 0x48, Size::Word), 0, "APB1ENR reset");
        assert_eq!(chip.read(pwr + 0x14, Size::Word), 0xC, "SR1 SPF, CKF");
        chip.write(pwr + 0x04, Size::Word, 0xC);
        assert_eq!(chip.read(pwr + 0x14, Size::Word), 0);
        // PDDS 10 is standby, not modelled: a plain sleep.
        chip.write(pwr, Size::Word, 0x40);
        assert!(chip.sleep(true));
        assert_eq!((chip.hclk_hz(), chip.stops), (8_000_000, 1));
    }

    #[test]
    fn the_power_meter_follows_run_sleep_and_stop() {
        let mut chip = Wb32::new();
        // MHSI's 8 MHz from reset, MHSI and FHSI on: awake 1/4 s, asleep
        // 1/4 s, stopped 1/2 s (PDDS 00 from reset, nothing pending).
        chip.advance(TICK_HZ / 4);
        assert!(chip.sleep(false));
        chip.advance(TICK_HZ / 2);
        chip.woke();
        assert!(chip.sleep(true));
        chip.advance(TICK_HZ);
        chip.woke();
        let e = chip.power().since(&Meter::default()).estimate();
        assert_eq!((e.awake, e.asleep, e.stopped), (0.25, 0.25, 0.5));
        // Run 1.12 mA x 1/4, stop 35 uA x 1/2, MHSI + FHSI 80 uA x 1/2.
        assert!((e.floor_ma - 0.3375).abs() < 1e-9, "{e}");
        assert!((e.sleep_as_run_ma - 0.6175).abs() < 1e-9, "{e}");
    }

    /// RM figure 10-1: a trigger sets PR only through IMR, and pulses the
    /// core's event input through EMR; the NVIC line follows PR.
    #[test]
    fn exti_pends_through_imr_and_sends_events_through_emr() {
        let mut chip = Wb32::new();
        let exti = GPIO_BASE + 0x1800;
        apb1_on(&mut chip, 1 << 9);
        let fall = |chip: &mut Wb32| {
            chip.write(GPIO_BASE + 0xFF4, Size::Word, 1 << 16);
            chip.write(GPIO_BASE + 0xFF4, Size::Word, 0);
        };
        chip.write(exti + 0x0C, Size::Word, 1);
        fall(&mut chip);
        assert_eq!(chip.read(exti + 0x14, Size::Word), 0, "masked: no PR");
        assert_eq!(chip.gpio.irq_level(), 0);
        assert!(!chip.irq_lines().event);
        chip.write(exti + 0x04, Size::Word, 1);
        fall(&mut chip);
        assert_eq!(chip.read(exti + 0x14, Size::Word), 0, "an event sets no PR");
        assert!(chip.irq_lines().event);
        assert!(!chip.irq_lines().event, "a pulse");
        chip.write(exti + 0x04, Size::Word, 0);
        chip.write(exti, Size::Word, 1);
        fall(&mut chip);
        assert_eq!(chip.read(exti + 0x14, Size::Word), 1);
        assert_eq!(chip.gpio.irq_level(), 1 << 6, "EXTI0");
        chip.write(exti, Size::Word, 0);
        assert_eq!(chip.gpio.irq_level(), 1 << 6, "PR still pending");
        chip.write(exti + 0x14, Size::Word, 1);
        assert_eq!(chip.gpio.irq_level(), 0);
        // SWIER triggers the same way.
        chip.write(exti + 0x10, Size::Word, 1 << 1);
        assert_eq!(chip.read(exti + 0x14, Size::Word), 0);
        // 19 lines.
        chip.write(exti + 0x04, Size::Word, u32::MAX);
        assert_eq!(chip.read(exti + 0x04, Size::Word), 0x7_FFFF);
        // In stop, a WFE ends on an event line.
        chip.write(exti + 0x04, Size::Word, 1);
        assert!(chip.sleep(true));
        fall(&mut chip);
        let lines = chip.irq_lines();
        assert!(lines.event && lines.level == 0, "{lines:?}");
    }
}
