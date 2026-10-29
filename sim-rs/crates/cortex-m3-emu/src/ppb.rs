//! The private peripheral bus (0xE000_0000..0xE00F_FFFF): the NVIC, SCB,
//! SysTick and DWT (ARM DDI 0403E chapter B3; the Cortex-M3 TRM for the
//! implementation values). Exception state (enabled, pending, active,
//! priorities) lives here; `exception.rs` acts on it.
//!
//! Anything else in the PPB (ITM, FPB, the MPU's regions, the ID registers)
//! reads as 0, ignores writes, and is reported, since our firmware has no
//! business there.

use crate::bus::Size;
use crate::{Config, CoreDiag};

/// Exceptions as bits: bit n is exception n (16 + IRQ).
pub type ExcSet = u128;

pub const SCS: u32 = 0xE000_E000;
pub const DWT: u32 = 0xE000_1000;

/// The Cortex-M3 r2p0 CPUID (the vendor header's __CM3_REV 0x0200).
pub const CPUID: u32 = 0x412F_C230;

pub struct Ppb {
    config: Config,
    pub enabled: ExcSet,
    pub pending: ExcSet,
    pub active: ExcSet,
    /// Priority by exception number; Reset/NMI/HardFault are fixed.
    pub priority: [u8; 16 + 64],
    /// IRQ lines held high, from the chip.
    pub irq_level: u64,
    pub vtor: u32,
    pub prigroup: u8,
    pub scr: u32,
    pub ccr: u32,
    pub shcsr: u32,
    pub cfsr: u32,
    pub hfsr: u32,
    pub mmfar: u32,
    pub bfar: u32,
    pub demcr: u32,
    pub mpu_ctrl: u32,
    pub mpu_rnr: u32,
    /// AIRCR.SYSRESETREQ was written: the core picks it up.
    pub reset_request: bool,
    /// An exception went pending with SCR.SEVONPEND set.
    pub sev: bool,
    /// The core's IPSR, mirrored for ICSR.VECTACTIVE.
    pub ipsr: u16,
    pub systick: SysTick,
    dwt_ctrl: u32,
    /// CYCCNT at `cyc_since` (and frozen there while counting is off).
    cyc_base: u32,
    cyc_since: u64,
    hclk_hz: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SysTick {
    pub csr: u32,
    pub rvr: u32,
    /// The counter's value at `since`; meaningful while enabled.
    value: u32,
    since: u64,
    /// When the counter next reaches 0 (ticks).
    next_zero: u64,
}

const CSR_ENABLE: u32 = 1;
const CSR_TICKINT: u32 = 2;
/// The processor clock; clear, the external reference (the WB32's HCLK / 8,
/// RM2905025 7.2.3).
const CSR_CLKSOURCE: u32 = 4;
const CSR_COUNTFLAG: u32 = 1 << 16;

impl Ppb {
    pub fn new(config: Config) -> Self {
        Ppb {
            config,
            enabled: 0,
            pending: 0,
            active: 0,
            priority: [0; 80],
            irq_level: 0,
            vtor: 0,
            prigroup: 0,
            scr: 0,
            // STKALIGN resets to 1 from r2p0.
            ccr: 1 << 9,
            shcsr: 0,
            cfsr: 0,
            hfsr: 0,
            mmfar: 0,
            bfar: 0,
            demcr: 0,
            mpu_ctrl: 0,
            mpu_rnr: 0,
            reset_request: false,
            sev: false,
            ipsr: 0,
            systick: SysTick::default(),
            dwt_ctrl: 0,
            cyc_base: 0,
            cyc_since: 0,
            hclk_hz: crate::DEFAULT_HCLK_HZ,
        }
    }

    pub fn reset(&mut self, now: u64) {
        let hz = self.hclk_hz;
        *self = Ppb::new(self.config);
        self.hclk_hz = hz;
        self.cyc_since = now;
    }

    pub fn stkalign(&self) -> bool {
        self.ccr & (1 << 9) != 0
    }

    pub fn unalign_trp(&self) -> bool {
        self.ccr & (1 << 3) != 0
    }

    pub fn div_0_trp(&self) -> bool {
        self.ccr & (1 << 4) != 0
    }

    pub fn sleepdeep(&self) -> bool {
        self.scr & 4 != 0
    }

    pub fn sleeponexit(&self) -> bool {
        self.scr & 2 != 0
    }

    pub fn sevonpend(&self) -> bool {
        self.scr & 16 != 0
    }

    /// Makes exception `exc` pending.
    pub fn pend(&mut self, exc: u16) {
        let bit = 1u128 << exc;
        if self.pending & bit == 0 && self.sevonpend() {
            self.sev = true;
        }
        self.pending |= bit;
    }

    /// The external inputs: a line held high pends its IRQ unless it's being
    /// handled (it pends again on return if still high); a pulse always does.
    pub fn irq_inputs(&mut self, level: u64, raised: u64) {
        self.irq_level = level;
        let want = ((level as u128 & !(self.active >> 16)) | raised as u128) << 16;
        let new = want & !self.pending;
        if new != 0 {
            if self.sevonpend() {
                self.sev = true;
            }
            self.pending |= new;
        }
    }

    // ---- SysTick and DWT, both clocked from virtual time ----

    /// The core clock: SysTick and CYCCNT count it.
    pub fn hclk_hz(&self) -> u64 {
        self.hclk_hz
    }

    /// The core clock changes at `now`: the counters carry on from where
    /// they are at the new rate.
    pub fn set_hclk(&mut self, hz: u64, now: u64) {
        let value = self.systick_value(now);
        let cyc = self.cyccnt(now);
        self.hclk_hz = hz;
        if self.systick.csr & CSR_ENABLE != 0 {
            self.systick_restart(value, now);
        }
        self.cyc_base = cyc;
        self.cyc_since = now;
    }

    fn systick_hz(&self) -> u64 {
        if self.systick.csr & CSR_CLKSOURCE != 0 {
            self.hclk_hz
        } else {
            self.hclk_hz / 8
        }
    }

    /// Ticks until `clocks` SysTick clocks have passed.
    fn systick_ticks(&self, clocks: u64) -> u64 {
        to_ticks(clocks, self.systick_hz())
    }

    fn systick_period(&self, reload: u32) -> u64 {
        self.systick_ticks(reload as u64 + 1)
    }

    /// Sets the counter to `value` as of `now`, and when it next reaches 0.
    fn systick_restart(&mut self, value: u32, now: u64) {
        let rvr = self.systick.rvr;
        let next_zero = if value == 0 {
            // It reloads on the next clock, then counts down from RVR.
            if rvr == 0 {
                u64::MAX
            } else {
                now + self.systick_ticks(rvr as u64 + 1)
            }
        } else {
            now + self.systick_ticks(value as u64)
        };
        let st = &mut self.systick;
        st.value = value;
        st.since = now;
        st.next_zero = next_zero;
    }

    fn systick_value(&self, now: u64) -> u32 {
        let st = &self.systick;
        if st.csr & CSR_ENABLE == 0 {
            return st.value;
        }
        let clocks = to_clocks(now - st.since, self.systick_hz());
        if clocks <= st.value as u64 {
            return st.value - clocks as u32;
        }
        if st.rvr == 0 {
            return 0;
        }
        // Past zero: reload, then count down, with a period of RVR + 1.
        let after = (clocks - st.value as u64 - 1) % (st.rvr as u64 + 1);
        st.rvr - after as u32
    }

    /// Fires what's due at `now`: SysTick reaching zero.
    pub fn tick(&mut self, now: u64) {
        let st = self.systick;
        if st.csr & CSR_ENABLE == 0 || now < st.next_zero {
            return;
        }
        self.systick.csr |= CSR_COUNTFLAG;
        if st.csr & CSR_TICKINT != 0 {
            self.pend(15);
        }
        // Rebase at the zero just passed, so later reads follow RVR.
        let zero = st.next_zero;
        self.systick.value = 0;
        self.systick.since = zero;
        self.systick.next_zero = if st.rvr == 0 {
            u64::MAX
        } else {
            zero + self.systick_period(st.rvr)
        };
        if self.systick.next_zero <= now {
            // A long jump (asleep with SysTick off its interrupt): catch up.
            let p = self.systick_period(st.rvr);
            self.systick.next_zero += (now - self.systick.next_zero) / p * p + p;
        }
    }

    /// When `tick` next has something to do.
    pub fn next_event(&self) -> u64 {
        if self.systick.csr & CSR_ENABLE != 0 {
            self.systick.next_zero
        } else {
            u64::MAX
        }
    }

    fn cyccnt(&self, now: u64) -> u32 {
        if self.dwt_ctrl & 1 == 0 {
            return self.cyc_base;
        }
        self.cyc_base
            .wrapping_add(to_clocks(now - self.cyc_since, self.hclk_hz) as u32)
    }

    // ---- register access ----

    pub fn read(&mut self, addr: u32, size: Size, now: u64, diags: &mut Vec<CoreDiag>) -> u32 {
        let word = self.read_word(addr & !3, now, diags);
        let shift = (addr & 3) * 8;
        (word >> shift) & size.mask()
    }

    pub fn write(
        &mut self,
        addr: u32,
        size: Size,
        value: u32,
        now: u64,
        diags: &mut Vec<CoreDiag>,
    ) {
        let shift = (addr & 3) * 8;
        let lanes = size.mask() << shift;
        let value = (value & size.mask()) << shift;
        let a = addr & !3;
        // Byte-wise priority registers: only the lanes written change.
        if (SCS + 0x400..SCS + 0x440).contains(&a) || (SCS + 0xD18..SCS + 0xD24).contains(&a) {
            for lane in 0..4 {
                if lanes >> (lane * 8) & 0xFF != 0 {
                    let exc = if a >= SCS + 0xD18 {
                        (a - (SCS + 0xD18)) as usize + 4 + lane
                    } else {
                        (a - (SCS + 0x400)) as usize + 16 + lane
                    };
                    self.priority[exc] = (value >> (lane * 8)) as u8 & self.config.prio_mask;
                }
            }
            return;
        }
        if size != Size::Word && !matches!(a - SCS, 0x100..=0x2FF) {
            diags.push(CoreDiag::Ppb {
                addr,
                write: Some(value >> shift),
                note: "sub-word access",
            });
        }
        self.write_word(a, value, lanes, now, diags);
    }

    fn read_word(&mut self, a: u32, now: u64, diags: &mut Vec<CoreDiag>) -> u32 {
        let irq_word = |set: ExcSet, i: u32| (set >> (16 + 32 * i)) as u32;
        match a {
            DWT => self.dwt_ctrl | 0x4000_0000, // NUMCOMP 4
            0xE000_1004 => self.cyccnt(now),
            _ if a < SCS => {
                diags.push(CoreDiag::Ppb {
                    addr: a,
                    write: None,
                    note: "not modelled",
                });
                0
            }
            _ => match a - SCS {
                0x004 => 1, // ICTR: 64 interrupt lines
                0x010 => {
                    let v = self.systick.csr;
                    self.systick.csr &= !CSR_COUNTFLAG;
                    v
                }
                0x014 => self.systick.rvr,
                0x018 => self.systick_value(now),
                // CALIB: no reference clock, no exact 10 ms value.
                0x01C => 0xC000_0000,
                o @ (0x100 | 0x104) => irq_word(self.enabled, (o - 0x100) / 4),
                o @ (0x180 | 0x184) => irq_word(self.enabled, (o - 0x180) / 4),
                o @ (0x200 | 0x204) => irq_word(self.pending, (o - 0x200) / 4),
                o @ (0x280 | 0x284) => irq_word(self.pending, (o - 0x280) / 4),
                o @ (0x300 | 0x304) => irq_word(self.active, (o - 0x300) / 4),
                o @ 0x400..=0x43F => {
                    let base = (o - 0x400) as usize + 16;
                    u32::from_le_bytes(self.priority[base..base + 4].try_into().unwrap())
                }
                0xD00 => CPUID,
                0xD04 => self.icsr(),
                0xD08 => self.vtor,
                0xD0C => 0xFA05_0000 | (self.prigroup as u32) << 8,
                0xD10 => self.scr,
                0xD14 => self.ccr,
                o @ 0xD18..=0xD23 => {
                    let base = (o - 0xD18) as usize + 4;
                    u32::from_le_bytes(self.priority[base..base + 4].try_into().unwrap())
                }
                0xD24 => self.shcsr_read(),
                0xD28 => self.cfsr,
                0xD2C => self.hfsr,
                0xD30 => 0,
                0xD34 => self.mmfar,
                0xD38 => self.bfar,
                0xD3C => 0,
                0xD88 => 0,      // CPACR: no coprocessors
                0xD90 => 8 << 8, // MPU_TYPE: 8 regions
                0xD94 => self.mpu_ctrl,
                0xD98 => self.mpu_rnr,
                0xDF0 => 0, // DHCSR: no debugger
                0xDFC => self.demcr,
                _ => {
                    diags.push(CoreDiag::Ppb {
                        addr: a,
                        write: None,
                        note: "not modelled",
                    });
                    0
                }
            },
        }
    }

    fn write_word(&mut self, a: u32, v: u32, lanes: u32, now: u64, diags: &mut Vec<CoreDiag>) {
        let irq_bits = |i: u32| ((v & lanes) as u128) << (16 + 32 * i);
        match a {
            DWT => {
                let was = self.cyccnt(now);
                self.dwt_ctrl = v & 0x0FFF_FFFF;
                self.cyc_base = was;
                self.cyc_since = now;
            }
            0xE000_1004 => {
                self.cyc_base = v;
                self.cyc_since = now;
            }
            _ if a < SCS => diags.push(CoreDiag::Ppb {
                addr: a,
                write: Some(v),
                note: "not modelled",
            }),
            _ => match a - SCS {
                0x010 => {
                    let was = self.systick.csr;
                    let value = self.systick_value(now);
                    self.systick.csr = (self.systick.csr & CSR_COUNTFLAG) | (v & 7);
                    let on = v & CSR_ENABLE != 0;
                    let rate_changed = (was ^ v) & CSR_CLKSOURCE != 0;
                    if on && (was & CSR_ENABLE == 0 || rate_changed) {
                        self.systick_restart(value, now);
                    } else if !on {
                        self.systick.value = value;
                    }
                }
                0x014 => self.systick.rvr = v & 0x00FF_FFFF,
                0x018 => {
                    // Any write clears the counter and COUNTFLAG.
                    self.systick.csr &= !CSR_COUNTFLAG;
                    self.systick_restart(0, now);
                }
                o @ (0x100 | 0x104) => self.enabled |= irq_bits((o - 0x100) / 4),
                o @ (0x180 | 0x184) => self.enabled &= !irq_bits((o - 0x180) / 4),
                o @ (0x200 | 0x204) => {
                    let set = irq_bits((o - 0x200) / 4);
                    for e in 16..80 {
                        if set & (1 << e) != 0 {
                            self.pend(e);
                        }
                    }
                }
                o @ (0x280 | 0x284) => self.pending &= !irq_bits((o - 0x280) / 4),
                0xD04 => self.icsr_write(v),
                0xD08 => self.vtor = v & 0xFFFF_FF80,
                0xD0C => {
                    if v >> 16 != 0x05FA {
                        return;
                    }
                    self.prigroup = ((v >> 8) & 7) as u8;
                    if v & 4 != 0 {
                        self.reset_request = true;
                    }
                    if v & 3 != 0 {
                        diags.push(CoreDiag::Ppb {
                            addr: a,
                            write: Some(v),
                            note: "VECTRESET/VECTCLRACTIVE (debug only)",
                        });
                    }
                }
                0xD10 => self.scr = v & 0x16,
                0xD14 => self.ccr = v & 0x31B,
                0xD24 => self.shcsr = v & 0x0007_0000,
                0xD28 => self.cfsr &= !v,
                0xD2C => self.hfsr &= !v,
                0xD34 => self.mmfar = v,
                0xD38 => self.bfar = v,
                0xD88 => {}
                0xD94 => {
                    self.mpu_ctrl = v & 7;
                    if v & 1 != 0 {
                        diags.push(CoreDiag::Ppb {
                            addr: a,
                            write: Some(v),
                            note: "MPU enabled; not enforced",
                        });
                    }
                }
                0xD98 => self.mpu_rnr = v & 0xFF,
                0xD9C..=0xDB8 => {} // region registers: not enforced
                0xDFC => self.demcr = v & 0x010F_03F1,
                0xF00 => self.pend(16 + (v & 0x3F) as u16),
                _ => diags.push(CoreDiag::Ppb {
                    addr: a,
                    write: Some(v),
                    note: "not modelled",
                }),
            },
        }
    }

    fn icsr(&self) -> u32 {
        let mut v = self.ipsr as u32; // VECTACTIVE
        if let Some((exc, _)) = self.highest_pending() {
            v |= (exc as u32) << 12;
        }
        if self.pending >> 16 != 0 {
            v |= 1 << 22; // ISRPENDING
        }
        if (self.active & !(1u128 << self.ipsr)) == 0 {
            v |= 1 << 11; // RETTOBASE
        }
        if self.pending & (1 << 15) != 0 {
            v |= 1 << 26;
        }
        if self.pending & (1 << 14) != 0 {
            v |= 1 << 28;
        }
        if self.pending & (1 << 2) != 0 {
            v |= 1 << 31;
        }
        v
    }

    /// An exception's priority: Reset -3, NMI -2, HardFault -1, else its
    /// programmed value.
    pub fn priority_of(&self, exc: u16) -> i16 {
        match exc {
            1 => -3,
            2 => -2,
            3 => -1,
            e => self.priority[e as usize] as i16,
        }
    }

    /// The group (preemption) part of a priority, per AIRCR.PRIGROUP.
    pub fn group(&self, p: i16) -> i16 {
        if p < 0 {
            p
        } else {
            (p as u16 & (0xFF << (self.prigroup + 1)) & 0xFF) as i16
        }
    }

    /// The pending exception that would be taken next, if any could be:
    /// lowest priority value, then lowest number; IRQs only when enabled.
    /// Returns it with its group priority.
    pub fn highest_pending(&self) -> Option<(u16, i16)> {
        let takeable = self.pending & (self.enabled | 0xFFFC);
        let mut best: Option<(i16, u16)> = None;
        let mut set = takeable;
        while set != 0 {
            let exc = set.trailing_zeros() as u16;
            set &= set - 1;
            let p = self.priority_of(exc);
            if best.is_none_or(|(bp, _)| p < bp) {
                best = Some((p, exc));
            }
        }
        best.map(|(p, exc)| (exc, self.group(p)))
    }

    fn icsr_write(&mut self, v: u32) {
        if v & (1 << 31) != 0 {
            self.pend(2);
        }
        if v & (1 << 28) != 0 {
            self.pend(14);
        }
        if v & (1 << 27) != 0 {
            self.pending &= !(1 << 14);
        }
        if v & (1 << 26) != 0 {
            self.pend(15);
        }
        if v & (1 << 25) != 0 {
            self.pending &= !(1 << 15);
        }
    }

    fn shcsr_read(&self) -> u32 {
        let bit = |set: ExcSet, e: u32| ((set >> e) & 1) as u32;
        self.shcsr
            | bit(self.active, 4)
            | bit(self.active, 5) << 1
            | bit(self.active, 6) << 3
            | bit(self.active, 11) << 7
            | bit(self.active, 12) << 8
            | bit(self.active, 14) << 10
            | bit(self.active, 15) << 11
            | bit(self.pending, 6) << 12
            | bit(self.pending, 4) << 13
            | bit(self.pending, 5) << 14
            | bit(self.pending, 11) << 15
    }
}

/// Ticks until `clocks` clocks at `hz` have passed (rounded up, so the
/// count has reached it then).
fn to_ticks(clocks: u64, hz: u64) -> u64 {
    (clocks as u128 * crate::TICK_HZ as u128).div_ceil(hz as u128) as u64
}

/// Whole clocks at `hz` in `ticks`.
fn to_clocks(ticks: u64, hz: u64) -> u64 {
    (ticks as u128 * hz as u128 / crate::TICK_HZ as u128) as u64
}
