//! GPIO ports A-D (0x4000_0000, 0x400 each), AFIO (0x4000_1400) and EXTI
//! (0x4000_1800), with a ROW2COL key matrix on the pins. A port of
//! Renode's sim/peripherals/gpio.py, register for register.
//!
//! Register layout per port (wb32fq95xx.h GPIO_TypeDef):
//!   0x00 MODER  0x04 OTYPER  0x08 OSPEEDR  0x0C PUPDR  0x10 IDR  0x14 ODR
//!   0x18 BSRR   0x1C LCKR    0x20 AFRL     0x24 AFRH   0x28 SMIT 0x2C CURRENT
//!   0x30 CFGMSK
//! WB32 quirk: writes to the configuration registers only change pins whose
//! CFGMSK bit is 0 (ChibiOS sets CFGMSK = ~mask before writing MODER etc.).
//!
//! Simulation control, as gpio.py, inside GPIOD's unused space (0x4000_0FF0..),
//! so the Renode tests' writes mean the same here:
//!   0xFF0 write: key   = (pressed << 16) | (row << 8) | col
//!   0xFF4 write: drive = (level << 16) | (port << 8) | pin, level 0 = low,
//!                1 = high, 2 = released (back to pull/floating)
//!   0xFF8 write: any value releases every key
//!
//! EXTI lines 0-15 follow the pin AFIO EXTICR selects. After every write
//! here (and every change of an input) each line's level is recomputed. As
//! RM figure 10-1 draws it, a trigger (an edge enabled in RTSR/FTSR, or
//! SWIER going to 1) sets PR only where IMR enables the line, and pulses
//! the core's event input where EMR does (gpio.py sets PR whatever IMR
//! says). Each NVIC line (EXTI0-4, EXTI9_5, EXTI15_10: IRQs 6-10, 16, 33 in
//! wb32fq95xx.h) is high while PR has a bit in its group. PR clears on
//! writing 1. The registers hold the RM's 19 lines (16-18, PVD, RTC alarm
//! and USB wakeup, have no source here).

use crate::chip::Write;
use std::collections::{BTreeMap, BTreeSet};

pub const PORTS: usize = 4;
pub const A: u8 = 0;
pub const B: u8 = 1;
pub const C: u8 = 2;
pub const D: u8 = 3;

/// A pin: (port, number).
pub type Pin = (u8, u8);

const MODER: u32 = 0x00;
const OTYPER: u32 = 0x04;
const PUPDR: u32 = 0x0C;
const IDR: u32 = 0x10;
const ODR: u32 = 0x14;
const BSRR: u32 = 0x18;
const AFRL: u32 = 0x20;
const AFRH: u32 = 0x24;
const CFGMSK: u32 = 0x30;
/// The configuration registers CFGMSK guards, and their field width.
const CFG_REGS: [(u32, u32); 8] = [
    (0x00, 2),
    (0x04, 1),
    (0x08, 2),
    (0x0C, 2),
    (0x20, 4),
    (0x24, 4),
    (0x28, 1),
    (0x2C, 2),
];

const AFIO: u32 = 0x1400;
const EXTI: u32 = 0x1800;
const IMR: usize = 0;
const EMR: usize = 1;
const RTSR: usize = 2;
const FTSR: usize = 3;
const SWIER: usize = 4;
const PR: usize = 5;
/// The RM's 19 lines (10.5: bits 31:19 reserved).
const LINES: u32 = (1 << 19) - 1;
/// NVIC line and the EXTI lines feeding it.
const EXTI_IRQS: [(u32, u32); 7] = [
    (6, 1 << 0),
    (7, 1 << 1),
    (8, 1 << 2),
    (9, 1 << 3),
    (10, 1 << 4),
    (16, 0x03E0),
    (33, 0xFC00),
];

/// A ROW2COL key matrix: a held key pulls its row low while its column
/// drives low.
#[derive(Clone, Debug, Default)]
pub struct Matrix {
    pub rows: Vec<Pin>,
    pub cols: Vec<Pin>,
}

#[derive(Clone, Debug, Default)]
pub struct Gpio {
    /// Per port, the registers at 0x00..=0x30 by offset / 4 (IDR and BSRR
    /// unused).
    regs: [[u32; 13]; PORTS],
    exticr: [u32; 4],
    /// IMR, EMR, RTSR, FTSR, SWIER, PR.
    exti: [u32; 6],
    line_last: [Option<bool>; 16],
    irq_level: u64,
    /// An event pulse to the core since `take_event`.
    event: bool,
    pub matrix: Matrix,
    /// Keys held: (row, col).
    pub pressed: BTreeSet<(u8, u8)>,
    /// Pins an outside level holds, overriding the chip.
    pub driven: BTreeMap<Pin, bool>,
    /// Pins something outside the chip watches for edges (a chip select).
    pub watched: Vec<Watch>,
}

/// A watched pin: its level after the last update, and its rising edges.
#[derive(Clone, Copy, Debug)]
pub struct Watch {
    pub pin: Pin,
    pub level: bool,
    pub rising: u32,
}

impl Gpio {
    pub fn new(matrix: Matrix) -> Self {
        Gpio {
            matrix,
            ..Default::default()
        }
    }

    /// A system reset: registers back to 0. What's outside the chip (keys,
    /// driven pins, watches) stays.
    pub fn reset(&mut self) {
        *self = Gpio {
            matrix: std::mem::take(&mut self.matrix),
            pressed: std::mem::take(&mut self.pressed),
            driven: std::mem::take(&mut self.driven),
            watched: std::mem::take(&mut self.watched),
            ..Default::default()
        };
        for w in &mut self.watched {
            w.level = true;
        }
    }

    /// RCC's reset of one port (RM 7.3.22): its registers back to 0.
    pub fn reset_port(&mut self, port: u8) {
        self.regs[port as usize] = Default::default();
        self.update();
    }

    /// RCC's reset of AFIO (EXTICR) or of EXTI.
    pub fn reset_afio(&mut self) {
        self.exticr = [0; 4];
        self.update();
    }

    pub fn reset_exti(&mut self) {
        self.exti = [0; 6];
        self.update();
    }

    /// Starts counting `pin`'s rising edges. It starts out high (as
    /// gpio.py's chip select), so the first rise counts once it was low.
    pub fn watch(&mut self, pin: Pin) {
        self.watched.push(Watch {
            pin,
            level: true,
            rising: 0,
        });
    }

    /// A watched pin's level and rising edges so far.
    pub fn watched(&self, pin: Pin) -> Option<Watch> {
        self.watched.iter().find(|w| w.pin == pin).copied()
    }

    fn reg(&self, port: u8, off: u32) -> u32 {
        self.regs[port as usize][(off / 4) as usize]
    }

    fn mode(&self, (port, pin): Pin) -> u32 {
        (self.reg(port, MODER) >> (pin * 2)) & 3 // 0 in, 1 out, 2 af, 3 analog
    }

    /// The alternate function a pin is set to (AFRL/AFRH), if MODER puts
    /// it in alternate-function mode.
    pub fn alternate(&self, p: Pin) -> Option<u8> {
        let (port, pin) = p;
        let afr = if pin < 8 { AFRL } else { AFRH };
        (self.mode(p) == 2).then(|| ((self.reg(port, afr) >> ((pin % 8) * 4)) & 0xF) as u8)
    }

    fn out_level(&self, (port, pin): Pin) -> bool {
        (self.reg(port, ODR) >> pin) & 1 != 0
    }

    fn drives_low(&self, p: Pin) -> bool {
        self.mode(p) == 1 && !self.out_level(p)
    }

    /// The level on a pin, as the chip would read it.
    pub fn pin_level(&self, p: Pin) -> bool {
        let (port, pin) = p;
        if let Some(&level) = self.driven.get(&p) {
            return level;
        }
        // An open-drain output set high doesn't drive: it reads as its pull.
        let open_drain = (self.reg(port, OTYPER) >> pin) & 1 != 0;
        if self.mode(p) == 1 && !(open_drain && self.out_level(p)) {
            return self.out_level(p);
        }
        if let Some(row) = self.matrix.rows.iter().position(|&r| r == p) {
            let held = self.pressed.iter().any(|&(r, c)| {
                r as usize == row
                    && self
                        .matrix
                        .cols
                        .get(c as usize)
                        .is_some_and(|&col| self.drives_low(col))
            });
            if held {
                return false;
            }
        }
        // Pull-up reads high; pull-down or floating, low.
        (self.reg(port, PUPDR) >> (pin * 2)) & 3 == 1
    }

    fn idr(&self, port: u8) -> u32 {
        (0..16).fold(0, |v, pin| v | (self.pin_level((port, pin)) as u32) << pin)
    }

    /// Bit n: IRQ n is held high.
    /// EXTI PR: the lines with a request pending.
    pub fn exti_pending(&self) -> u32 {
        self.exti[PR]
    }

    /// Whether an EMR line pulsed the core's event input since last asked.
    pub fn take_event(&mut self) -> bool {
        std::mem::take(&mut self.event)
    }

    /// Lines triggered (figure 10-1): the pending bits behind IMR, an event
    /// behind EMR.
    fn trigger(&mut self, lines: u32) {
        self.exti[PR] |= lines & self.exti[IMR];
        self.event |= lines & self.exti[EMR] != 0;
    }

    pub fn irq_level(&self) -> u64 {
        self.irq_level
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if off >= EXTI {
            let i = ((off - EXTI) / 4) as usize;
            return if (off - EXTI) % 4 == 0 {
                self.exti.get(i).copied().unwrap_or(0)
            } else {
                0
            };
        }
        if off >= AFIO {
            return match off - AFIO {
                o @ (0x08 | 0x0C | 0x10 | 0x14) => self.exticr[((o - 8) / 4) as usize],
                _ => 0,
            };
        }
        if off >= 0xFF0 {
            return 0;
        }
        let (port, o) = ((off / 0x400) as u8, off % 0x400);
        match o {
            IDR => self.idr(port),
            BSRR => 0,
            o if o <= CFGMSK && o % 4 == 0 => self.reg(port, o),
            _ => 0,
        }
    }

    /// A write; the ones gpio.py logs as "MMIO W" go to `log`.
    pub fn write(&mut self, off: u32, v: u32, log: &mut impl FnMut(Write)) {
        if off >= EXTI {
            let o = off - EXTI;
            log(Write {
                block: "EXTI",
                off: o,
                value: v,
            });
            let i = (o / 4) as usize;
            if o % 4 == 0 && i < self.exti.len() {
                let v = v & LINES;
                match i {
                    PR => self.exti[PR] &= !v,
                    SWIER => {
                        self.trigger(v & !self.exti[SWIER]);
                        self.exti[SWIER] = v;
                    }
                    _ => self.exti[i] = v,
                }
            }
        } else if off >= AFIO {
            let o = off - AFIO;
            let v = v & 0xFFFF;
            log(Write {
                block: "AFIO",
                off: o,
                value: v,
            });
            if let 0x08 | 0x0C | 0x10 | 0x14 = o {
                self.exticr[((o - 8) / 4) as usize] = v;
            }
        } else if off >= 0x1000 {
        } else if off >= 0xFF0 {
            match off {
                0xFF0 => {
                    let key = ((v >> 8) as u8, v as u8);
                    if (v >> 16) & 1 != 0 {
                        self.pressed.insert(key);
                    } else {
                        self.pressed.remove(&key);
                    }
                }
                0xFF4 => {
                    let pin = ((v >> 8) as u8, v as u8);
                    match (v >> 16) & 0xFF {
                        2 => {
                            self.driven.remove(&pin);
                        }
                        level => {
                            self.driven.insert(pin, level != 0);
                        }
                    }
                }
                0xFF8 => self.pressed.clear(),
                _ => {}
            }
        } else {
            let (port, o) = ((off / 0x400) as usize, off % 0x400);
            log(Write {
                block: ["GPIOA", "GPIOB", "GPIOC", "GPIOD"][port],
                off: o,
                value: v,
            });
            let r = &mut self.regs[port];
            if let Some(&(_, bits)) = CFG_REGS.iter().find(|c| c.0 == o) {
                let mut m = 0u32;
                for pin in 0..16u32 {
                    if (r[12] >> pin) & 1 == 0 {
                        m |= field_mask(o, bits, pin);
                    }
                }
                r[(o / 4) as usize] = (r[(o / 4) as usize] & !m) | (v & m);
            } else if o == BSRR {
                // Low half sets, high half resets (set wins).
                let odr = &mut r[(ODR / 4) as usize];
                *odr = (*odr & !(v >> 16)) | (v & 0xFFFF);
            } else if o != IDR && o <= CFGMSK && o % 4 == 0 {
                r[(o / 4) as usize] = v;
            }
        }
        self.update();
    }

    fn update_watched(&mut self) {
        for i in 0..self.watched.len() {
            let level = self.pin_level(self.watched[i].pin);
            let w = &mut self.watched[i];
            w.rising += (level && !w.level) as u32;
            w.level = level;
        }
    }

    /// Recomputes the EXTI lines after anything that can move a pin.
    pub fn update(&mut self) {
        for line in 0..16u8 {
            let port = (self.exticr[(line / 4) as usize] >> ((line % 4) * 4)) & 0xF;
            let level = (port as usize) < PORTS && self.pin_level((port as u8, line));
            let old = self.line_last[line as usize].replace(level);
            let bit = 1 << line;
            if old.is_some_and(|o| o != level)
                && ((level && self.exti[RTSR] & bit != 0) || (!level && self.exti[FTSR] & bit != 0))
            {
                self.trigger(bit);
            }
        }
        self.update_watched();
        let active = self.exti[PR];
        self.irq_level = EXTI_IRQS
            .iter()
            .filter(|(_, mask)| active & mask != 0)
            .fold(0, |l, (irq, _)| l | 1 << irq);
    }
}

fn field_mask(off: u32, bits: u32, pin: u32) -> u32 {
    let pin = match off {
        0x20 if pin < 8 => pin,
        0x24 if pin >= 8 => pin - 8,
        0x20 | 0x24 => return 0,
        _ => pin,
    };
    (((1u64 << bits) - 1) as u32) << (pin * bits)
}
