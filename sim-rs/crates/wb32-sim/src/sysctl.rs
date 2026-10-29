//! The system control block, 0x4001_0000..0x4001_0FFF: PWR 0x000, ANCTL
//! 0x400, IWDG 0x800, RCC 0xC00 (offsets from wb32fq95xx.h). A port of
//! ../../sim/peripherals/sysctl.py: registers remember what was written,
//! and the status bits hardware would set follow the enables (ANCTL
//! MHSISR/FHSISR/LSISR/HSESR bit 0, PLLSR = 3 once PLLON is set).
//! ANCTL is only writable while PWR ANAKEY1/2 hold 0x03/0x0C (as
//! ChibiOS's hal_lld.c brackets every ANCTL access); locked writes are
//! dropped and reported, which tests treat as a firmware bug.

use crate::chip::Write;
use std::collections::BTreeMap;

const PWR: u32 = 0x000;
const ANCTL: u32 = 0x400;
const IWDG: u32 = 0x800;
const ANAKEY1: u32 = PWR + 0x28;
const ANAKEY2: u32 = PWR + 0x2C;
const BLOCKS: [&str; 4] = ["PWR", "ANCTL", "IWDG", "RCC"];

#[derive(Clone, Debug, Default)]
pub struct Sysctl {
    regs: BTreeMap<u32, u32>,
}

impl Sysctl {
    pub fn reset(&mut self) {
        self.regs.clear();
    }

    pub fn reg(&self, off: u32) -> u32 {
        self.regs.get(&off).copied().unwrap_or(0)
    }

    fn anctl_unlocked(&self) -> bool {
        self.reg(ANAKEY1) == 0x03 && self.reg(ANAKEY2) == 0x0C
    }

    /// The ready flags hardware sets, by ANCTL offset.
    fn status(&self, off: u32) -> Option<u32> {
        let enabled = |o: u32, mask: u32| self.reg(ANCTL + o) & mask != 0;
        Some(match off {
            0x30 => 1,                          // MHSI: on from reset
            0x3C => enabled(0x38, 1) as u32,    // FHSI
            0x48 => enabled(0x44, 1) as u32,    // LSI
            0x58 => enabled(0x4C, 3) as u32,    // HSE: HSEON or BYPASS
            0x7C => 3 * enabled(0x78, 1) as u32, // PLL
            _ => return None,
        })
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if (ANCTL..IWDG).contains(&off)
            && let Some(s) = self.status(off - ANCTL)
        {
            return s;
        }
        self.reg(off)
    }

    /// The block name and offset inside it, as sysctl.py logs them.
    pub fn block(off: u32) -> (&'static str, u32) {
        (BLOCKS[(off / 0x400) as usize], off % 0x400)
    }

    /// Writes; false if ANCTL was locked and the write dropped.
    pub fn write(&mut self, off: u32, v: u32, log: &mut impl FnMut(Write)) -> bool {
        if (ANCTL..IWDG).contains(&off) && !self.anctl_unlocked() {
            return false;
        }
        self.regs.insert(off, v);
        let (block, off) = Self::block(off);
        log(Write { block, off, value: v });
        true
    }
}
