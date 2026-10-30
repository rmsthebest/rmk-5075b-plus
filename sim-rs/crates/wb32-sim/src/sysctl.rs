//! The system control block, 0x4001_0000..0x4001_0FFF: PWR 0x000, ANCTL
//! 0x400, IWDG 0x800, RCC 0xC00 (offsets from wb32fq95xx.h). Registers
//! remember what was written, from their reset values (RM 5.4, 7.3, 11.4).
//! ANCTL is only writable while PWR ANAKEY1/2 hold 0x03/0x0C (as
//! ChibiOS's hal_lld.c brackets every ANCTL access), and the keys read 0;
//! locked writes are dropped and reported, which tests treat as a firmware
//! bug.
//!
//! The clock tree (RM 7.2, 11.3.5): MAINCLK is MHSI (8 MHz) from reset,
//! FHSI (48 MHz), HSE (the board's 12 MHz crystal) or the PLL (MHSI or HSE,
//! through RCC_PLLPRE, times ANCTL_PLLCR's factor). Writing MAINCLKUEN
//! switches to MAINCLKSRC's source if it's stable; otherwise nothing
//! changes and it's reported. AHB divides MAINCLK by AHBPRE; APB1 and APB2
//! divide MAINCLK too (RM 7.2.14), each off until its SRCEN. An
//! oscillator's ready flag rises its start-up time after it's enabled (DS
//! 5.2.4, 5.2.5, at their maximums), the PLL's once its input is stable.
//!
//! A system reset keeps PWR SR1, GPREG0/1 and CFGR (RM 5.4.5-5.4.8: only
//! a power-on reset clears them).
//!
//! Out of a stop (RM 5.3.4) every clock configuration is back at its reset
//! value: RCC's registers but its reset bits, and the oscillators (MHSI and
//! FHSI on, starting up again; HSE and the PLL off); MAINCLK is MHSI. PWR
//! SR1 then has SPF and CKF set, which writing CSPF/CCKF to CR1 clears.

use crate::chip::Write;
use cortex_m3_emu::TICK_HZ;
use std::collections::BTreeMap;

const PWR: u32 = 0x000;
const ANCTL: u32 = 0x400;
const IWDG: u32 = 0x800;
const RCC: u32 = 0xC00;
const ANAKEY1: u32 = PWR + 0x28;
const ANAKEY2: u32 = PWR + 0x2C;
const PWR_CR0: u32 = PWR;
const PWR_CR1: u32 = PWR + 0x04;
const PWR_SR1: u32 = PWR + 0x14;
/// SR1: woke from a stop (SPF), with the clocks reset (CKF).
pub const SR1_SPF: u32 = 1 << 2;
pub const SR1_CKF: u32 = 1 << 3;
const BLOCKS: [&str; 4] = ["PWR", "ANCTL", "IWDG", "RCC"];

// RCC (RM 7.3).
const PLLPRE: u32 = RCC;
const PLLSRC: u32 = RCC + 0x04;
const MAINCLKSRC: u32 = RCC + 0x08;
const MAINCLKUEN: u32 = RCC + 0x0C;
const USBPRE: u32 = RCC + 0x14;
const AHBPRE: u32 = RCC + 0x18;
const APB1PRE: u32 = RCC + 0x1C;
const APB2PRE: u32 = RCC + 0x20;
pub const AHBENR1: u32 = RCC + 0x40;
pub const APB1ENR: u32 = RCC + 0x48;
pub const APB2ENR: u32 = RCC + 0x4C;
pub const AHBRSTR1: u32 = RCC + 0x88;
pub const APB1RSTR: u32 = RCC + 0x90;
pub const APB2RSTR: u32 = RCC + 0x94;
/// The APB bus matrices: "must be set for APB1 (APB2) peripheral's
/// operation" (RM 7.3.14, 7.3.15).
const BMX1EN: u32 = 1 << 15;
const BMX2EN: u32 = 1 << 11;
// ANCTL (RM 11.4): enables, and the status register each one sets.
const MHSIENR: u32 = ANCTL + 0x2C;
const FHSIENR: u32 = ANCTL + 0x38;
const LSIENR: u32 = ANCTL + 0x44;
const HSECR0: u32 = ANCTL + 0x4C;
const PLLCR: u32 = ANCTL + 0x74;
const PLLENR: u32 = ANCTL + 0x78;
const STATUS: [(u32, u32); 5] = [
    (MHSIENR, ANCTL + 0x30),
    (FHSIENR, ANCTL + 0x3C),
    (LSIENR, ANCTL + 0x48),
    (HSECR0, ANCTL + 0x58),
    (PLLENR, ANCTL + 0x7C),
];

/// Reset values that aren't 0.
const RESET: [(u32, u32); 7] = [
    (PWR + 0x20, 0x3BE),
    (MHSIENR, 1),
    (FHSIENR, 1),
    (RCC + 0x3C, 0x7B),
    (RCC + 0x40, 0x3D),
    (RCC + 0x44, 0x03),
    (RCC + 0x64, 0x01),
];
/// PWR SR1, GPREG0, GPREG1, CFGR: kept over a system reset.
const KEPT: [u32; 4] = [PWR + 0x14, PWR + 0x18, PWR + 0x1C, PWR + 0x20];

pub const MHSI_HZ: u64 = 8_000_000;
pub const FHSI_HZ: u64 = 48_000_000;
/// The board's crystal: ChibiOS's GENERIC_WB32_FQ95XX mcuconf.h
/// (WB32_HSECLK), which QMK is built with; its PLL set-up (HSE / 2 x 16)
/// then gives the 96 MHz the board's UART divider for 115200 assumes.
pub const HSE_HZ: u64 = 12_000_000;

const fn us(n: u64) -> u64 {
    n * TICK_HZ / 1_000_000
}

/// Start-up times by enable register: DS tables 5.2-6 to 5.2-9 at their
/// maximums. The HSE crystal's isn't in the DS: 2 ms is assumed (a board
/// check).
fn startup(enable: u32) -> u64 {
    match enable {
        MHSIENR => us(2),
        FHSIENR => TICK_HZ / 2_000_000,
        LSIENR => us(85),
        HSECR0 => us(2000),
        _ => us(200), // PLL lock
    }
}

/// The clocks the tree gives, in Hz (0: stopped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clocks {
    pub mainclk: u64,
    pub hclk: u64,
    pub pclk1: u64,
    pub pclk2: u64,
    /// MAINCLK through USBPRE: USB needs 48 MHz.
    pub usb: u64,
}

/// A peripheral's clock enable: its bus and its bit in that bus's enable
/// and reset registers (RM 7.3.12, 7.3.14, 7.3.15, 7.3.21-7.3.23).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Ahb(u32),
    Apb1(u32),
    Apb2(u32),
}

impl Gate {
    /// (enable register, reset register, bit).
    fn regs(self) -> (u32, u32, u32) {
        match self {
            Gate::Ahb(b) => (AHBENR1, AHBRSTR1, b),
            Gate::Apb1(b) => (APB1ENR, APB1RSTR, b),
            Gate::Apb2(b) => (APB2ENR, APB2RSTR, b),
        }
    }
}

/// What a write did, when not simply stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// ANCTL while locked: dropped.
    Locked,
    /// MAINCLKUEN while MAINCLKSRC's source isn't stable: no switch.
    NotReady { src: u32 },
}

#[derive(Clone, Debug)]
pub struct Sysctl {
    regs: BTreeMap<u32, u32>,
    /// When each enabled oscillator is stable (by enable register); the
    /// PLL's entry is when it was turned on, as it locks on its input.
    on: BTreeMap<u32, u64>,
    /// The source MAINCLK runs from (MAINCLKSRC's encoding).
    mainclk_src: u32,
    /// `clocks()`, kept up to date by every write (read per instruction).
    clocks: Clocks,
}

impl Default for Sysctl {
    /// Powered on.
    fn default() -> Self {
        let mut s = Sysctl {
            regs: BTreeMap::new(),
            on: BTreeMap::new(),
            mainclk_src: 0,
            clocks: Clocks {
                mainclk: MHSI_HZ,
                hclk: MHSI_HZ,
                pclk1: 0,
                pclk2: 0,
                usb: 0,
            },
        };
        s.reset();
        s
    }
}

impl Sysctl {
    /// A system reset: everything to its reset value but `KEPT`. The
    /// oscillators enabled from reset were running and stay stable.
    pub fn reset(&mut self) {
        let kept: Vec<(u32, u32)> = KEPT
            .iter()
            .filter_map(|&o| self.regs.get(&o).map(|&v| (o, v)))
            .collect();
        self.regs = RESET.into_iter().chain(kept).collect();
        self.on = [(MHSIENR, 0), (FHSIENR, 0)].into();
        self.mainclk_src = 0;
        self.clocks = self.compute_clocks();
    }

    /// SLEEPDEEP enters stop rather than standby: CR0 PDDS (bits 6:5) 00.
    pub fn deep_sleep_is_stop(&self) -> bool {
        self.reg(PWR_CR0) >> 5 & 3 == 0
    }

    /// Out of a stop at `now` (see the top of this file).
    pub fn stop_exit(&mut self, now: u64) {
        let rcc_kept = [AHBRSTR1, APB1RSTR, APB2RSTR].map(|o| (o, self.reg(o)));
        self.regs.retain(|&o, _| o < RCC);
        for (o, v) in RESET {
            if o >= RCC {
                self.regs.insert(o, v);
            }
        }
        self.regs.extend(rcc_kept);
        for (e, v) in [(MHSIENR, 1), (FHSIENR, 1), (HSECR0, 0), (PLLENR, 0)] {
            self.regs.insert(e, v);
        }
        self.on = [
            (MHSIENR, now + startup(MHSIENR)),
            (FHSIENR, now + startup(FHSIENR)),
        ]
        .into();
        self.mainclk_src = 0;
        *self.regs.entry(PWR_SR1).or_default() |= SR1_SPF | SR1_CKF;
        self.clocks = self.compute_clocks();
    }

    pub fn reg(&self, off: u32) -> u32 {
        self.regs.get(&off).copied().unwrap_or(0)
    }

    fn anctl_unlocked(&self) -> bool {
        self.reg(ANAKEY1) == 0x03 && self.reg(ANAKEY2) == 0x0C
    }

    /// When the oscillator `enable` controls is stable, if it's on.
    fn ready_at(&self, enable: u32) -> Option<u64> {
        let on = *self.on.get(&enable)?;
        if enable != PLLENR {
            return Some(on);
        }
        // The PLL locks once its input is there.
        if self.reg(PLLPRE) & 0x20 == 0 {
            return None;
        }
        let input = if self.reg(PLLSRC) & 1 != 0 {
            HSECR0
        } else {
            MHSIENR
        };
        Some(on.max(self.ready_at(input)?) + startup(PLLENR))
    }

    fn ready(&self, enable: u32, now: u64) -> bool {
        self.ready_at(enable).is_some_and(|t| t <= now)
    }

    /// When the next ready flag rises, for a chip that wakes a sleeping
    /// core on it (none does: firmware polls them).
    pub fn next_ready(&self, now: u64) -> Option<u64> {
        STATUS
            .iter()
            .filter_map(|&(e, _)| self.ready_at(e))
            .filter(|&t| t > now)
            .min()
    }

    /// A source's frequency (MAINCLKSRC's encoding), stable or not.
    fn source_hz(&self, src: u32) -> u64 {
        match src & 3 {
            0 => MHSI_HZ,
            1 => FHSI_HZ,
            2 => {
                let pre = self.reg(PLLPRE);
                let input = if self.reg(PLLSRC) & 1 != 0 {
                    HSE_HZ
                } else {
                    MHSI_HZ
                };
                let div = if pre & 1 != 0 {
                    ((pre >> 1) & 0xF) as u64 + 2
                } else {
                    1
                };
                let mul = [24, 20, 16, 12][((self.reg(PLLCR) >> 6) & 3) as usize];
                input / div * mul
            }
            _ => HSE_HZ,
        }
    }

    fn source_enable(src: u32) -> u32 {
        [MHSIENR, FHSIENR, PLLENR, HSECR0][(src & 3) as usize]
    }

    /// Whether the peripheral behind `g` has its register clock: its enable
    /// bit, and for an APB one the bus's clock (SRCEN) and its bus matrix
    /// (BMXnEN); and that its reset bit doesn't hold it in reset.
    pub fn clock_on(&self, g: Gate) -> bool {
        let (en, rst, bit) = g.regs();
        let bus = match g {
            Gate::Ahb(_) => true,
            Gate::Apb1(_) => self.clocks.pclk1 != 0 && self.reg(APB1ENR) & BMX1EN != 0,
            Gate::Apb2(_) => self.clocks.pclk2 != 0 && self.reg(APB2ENR) & BMX2EN != 0,
        };
        bus && self.reg(en) & 1 << bit != 0 && self.reg(rst) & 1 << bit == 0
    }

    /// The clocks as the registers now set them.
    pub fn clocks(&self) -> Clocks {
        self.clocks
    }

    fn compute_clocks(&self) -> Clocks {
        let mainclk = self.source_hz(self.mainclk_src);
        // RATIO n divides by n + 2 while DIVEN is set.
        let div = |v: u32| {
            if v & 1 != 0 {
                ((v >> 1) & 0x3F) as u64 + 2
            } else {
                1
            }
        };
        let apb = |v: u32| {
            if v & 0x80 != 0 { mainclk / div(v) } else { 0 }
        };
        // USBPRE (RM 7.3.5): SRCEN gates it; DIVEN divides by 2, 1.5 or 3.
        let usb = match self.reg(USBPRE) & 0xF {
            v if v & 8 == 0 => 0,
            v if v & 1 == 0 => mainclk,
            v if v & 6 == 0 => mainclk / 2,
            v if v & 6 == 4 => mainclk * 2 / 3,
            _ => mainclk / 3,
        };
        Clocks {
            mainclk,
            hclk: mainclk / div(self.reg(AHBPRE)),
            pclk1: apb(self.reg(APB1PRE)),
            pclk2: apb(self.reg(APB2PRE)),
            usb,
        }
    }

    pub fn read(&mut self, off: u32, now: u64) -> u32 {
        if let Some(&(e, _)) = STATUS.iter().find(|s| s.1 == off) {
            let ready = self.ready(e, now) as u32;
            // PLLSR: 3 when locked.
            return if e == PLLENR { 3 * ready } else { ready };
        }
        match off {
            ANAKEY1 | ANAKEY2 | MAINCLKUEN | PWR_CR1 => 0,
            _ => self.reg(off),
        }
    }

    /// The block name and offset inside it, as sysctl.py logs them.
    pub fn block(off: u32) -> (&'static str, u32) {
        (BLOCKS[(off / 0x400) as usize], off % 0x400)
    }

    /// Writes, unless refused.
    pub fn write(
        &mut self,
        off: u32,
        v: u32,
        now: u64,
        log: &mut impl FnMut(Write),
    ) -> Result<(), Refused> {
        if (ANCTL..IWDG).contains(&off) && !self.anctl_unlocked() {
            return Err(Refused::Locked);
        }
        let (block, o) = Self::block(off);
        log(Write {
            block,
            off: o,
            value: v,
        });
        if let Some(&(e, _)) = STATUS.iter().find(|s| s.0 == off) {
            let mask = if e == HSECR0 { 3 } else { 1 };
            let (was, is) = (self.reg(off) & mask != 0, v & mask != 0);
            if is && !was {
                let at = if e == PLLENR { now } else { now + startup(e) };
                self.on.insert(e, at);
            } else if was && !is {
                self.on.remove(&e);
            }
        }
        match off {
            PWR_CR1 => {
                *self.regs.entry(PWR_SR1).or_default() &= !(v & 0xF);
                return Ok(());
            }
            PWR_SR1 => return Ok(()),
            _ => {}
        }
        self.regs.insert(off, v);
        if off == MAINCLKUEN && v & 1 != 0 {
            let src = self.reg(MAINCLKSRC) & 3;
            if !self.ready(Self::source_enable(src), now) {
                return Err(Refused::NotReady { src });
            }
            self.mainclk_src = src;
        }
        self.clocks = self.compute_clocks();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(s: &mut Sysctl, off: u32, v: u32, now: u64) -> Result<(), Refused> {
        s.write(ANAKEY1, 3, now, &mut |_| {}).unwrap();
        s.write(ANAKEY2, 0xC, now, &mut |_| {}).unwrap();
        s.write(off, v, now, &mut |_| {})
    }

    #[test]
    fn reset_values_and_the_keys_read_0() {
        let mut s = Sysctl::default();
        assert_eq!(s.read(RCC + 0x3C, 0), 0x7B);
        assert_eq!(s.read(RCC + 0x40, 0), 0x3D);
        assert_eq!(s.read(PWR + 0x20, 0), 0x3BE);
        // MHSI and FHSI run from reset.
        assert_eq!((s.read(ANCTL + 0x30, 0), s.read(ANCTL + 0x3C, 0)), (1, 1));
        set(&mut s, ANAKEY1, 3, 0).unwrap();
        assert_eq!(s.read(ANAKEY1, 0), 0);
        let c = s.clocks();
        assert_eq!(
            (c.mainclk, c.hclk, c.pclk1, c.pclk2, c.usb),
            (MHSI_HZ, MHSI_HZ, 0, 0, 0)
        );
    }

    #[test]
    fn a_system_reset_keeps_pwr_sr1_gpreg_and_cfgr() {
        let mut s = Sysctl::default();
        for (o, v) in [(0x18, 0xAA), (0x1C, 0xBB), (0x20, 0x3B3), (0x08, 5)] {
            s.write(PWR + o, v, 0, &mut |_| {}).unwrap();
        }
        s.write(RCC + 0x48, 0x8100, 0, &mut |_| {}).unwrap();
        s.reset();
        assert_eq!(s.reg(PWR + 0x18), 0xAA);
        assert_eq!(s.reg(PWR + 0x1C), 0xBB);
        assert_eq!(s.reg(PWR + 0x20), 0x3B3);
        assert_eq!(s.reg(PWR + 0x08), 0);
        assert_eq!(s.reg(RCC + 0x48), 0);
        assert_eq!(Sysctl::default().reg(PWR + 0x20), 0x3BE);
    }

    /// QMK's set-up (sim/golden/qmk_clock_init.txt): HSE, then the PLL at
    /// HSE / 2 x 16 = 96 MHz, then MAINCLK to the PLL; APB1 and APB2 on,
    /// undivided.
    #[test]
    fn qmks_pll_set_up_gives_96_mhz_once_locked() {
        let mut s = Sysctl::default();
        let t0 = us(10);
        set(&mut s, RCC + 0x1C, 0x80, t0).unwrap();
        set(&mut s, ANCTL + 0x50, 2, t0).unwrap();
        set(&mut s, HSECR0, 1, t0).unwrap();
        set(&mut s, RCC + 0x20, 0x80, t0).unwrap();
        set(&mut s, PLLSRC, 1, t0).unwrap();
        set(&mut s, PLLPRE, 0x21, t0).unwrap();
        set(&mut s, PLLCR, 0x80, t0).unwrap();
        set(&mut s, PLLENR, 1, t0).unwrap();
        // The HSE crystal's 2 ms, then the PLL's 200 us.
        assert_eq!(s.read(ANCTL + 0x58, t0 + us(1999)), 0);
        assert_eq!(s.read(ANCTL + 0x58, t0 + us(2000)), 1);
        assert_eq!(s.read(ANCTL + 0x7C, t0 + us(2199)), 0);
        assert_eq!(s.next_ready(t0 + us(2000)), Some(t0 + us(2200)));
        set(&mut s, MAINCLKSRC, 2, t0).unwrap();
        assert_eq!(
            set(&mut s, MAINCLKUEN, 1, t0 + us(2199)),
            Err(Refused::NotReady { src: 2 })
        );
        assert_eq!(s.clocks().mainclk, MHSI_HZ);
        assert_eq!(s.read(ANCTL + 0x7C, t0 + us(2200)), 3);
        set(&mut s, MAINCLKUEN, 1, t0 + us(2200)).unwrap();
        assert_eq!(s.read(MAINCLKUEN, t0 + us(2200)), 0);
        let c = s.clocks();
        assert_eq!(
            (c.mainclk, c.hclk, c.pclk1, c.pclk2),
            (96_000_000, 96_000_000, 96_000_000, 96_000_000)
        );
        // USB: / 2 (QMK's 0x09), / 1.5, undivided.
        for (pre, usb) in [(0x09, 48_000_000), (0x0D, 64_000_000), (0x08, 96_000_000)] {
            set(&mut s, USBPRE, pre, 0).unwrap();
            assert_eq!(s.clocks().usb, usb, "USBPRE 0x{pre:x}");
        }
        // Dividers: AHB / 2, APB1 / 4.
        set(&mut s, AHBPRE, 1, 0).unwrap();
        set(&mut s, APB1PRE, 0x80 | 2 << 1 | 1, 0).unwrap();
        let c = s.clocks();
        assert_eq!((c.hclk, c.pclk1), (48_000_000, 24_000_000));
    }

    #[test]
    fn the_pll_doesnt_lock_without_its_input() {
        let mut s = Sysctl::default();
        set(&mut s, PLLSRC, 1, 0).unwrap();
        set(&mut s, PLLPRE, 0x21, 0).unwrap();
        set(&mut s, PLLENR, 1, 0).unwrap();
        assert_eq!(s.read(ANCTL + 0x7C, us(10_000)), 0, "HSE off");
        set(&mut s, PLLSRC, 0, 0).unwrap();
        set(&mut s, PLLPRE, 0x01, 0).unwrap();
        assert_eq!(s.read(ANCTL + 0x7C, us(10_000)), 0, "no SRCEN");
        set(&mut s, PLLPRE, 0x21, 0).unwrap();
        assert_eq!(s.read(ANCTL + 0x7C, us(10_000)), 3, "from MHSI");
    }
}
