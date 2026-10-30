//! TIM2, 0x4000_2000: an STM32-style general-purpose timer (CR1..CCR4,
//! wb32fq95xx.h TIM_TypeDef and TIM_* bits) on IRQ 21, clocked from PCLK1
//! while RCC gives TIM2 its clock (`set_clock`; 96 MHz with our clock
//! set-up, and stopped from reset). Written fresh rather than after Renode's
//! STM32_Timer, which differs from the chip (BOARD_FACTS.md):
//! - it's 16-bit: CNT, ARR and CCRx keep their low 16 bits;
//! - compare flags are set only when the counter wraps, with the update.
//!
//! Up-counting only. PSC is preloaded (it takes effect at the next update
//! event or UG); ARR takes effect at once. UG resets the counter and, unless
//! CR1.URS, sets UIF. OPM stops the counter at its update. The counter is
//! computed from the time since it was last folded in (`sync`).

use cortex_m3_emu::TICK_HZ;

const CR1: u32 = 0x00;
const DIER: u32 = 0x0C;
const SR: u32 = 0x10;
const EGR: u32 = 0x14;
const CNT: u32 = 0x24;
const PSC: u32 = 0x28;
const ARR: u32 = 0x2C;
const CEN: u32 = 1 << 0;
const URS: u32 = 1 << 2;
const OPM: u32 = 1 << 3;
const UIF: u32 = 1 << 0;
/// UIF, CC1IF-CC4IF.
const WRAP_FLAGS: u32 = 0x1F;
/// The flags DIER can turn into an interrupt: UIE, CC1IE-CC4IE, COMIE, TIE.
const IRQ_FLAGS: u32 = 0x7F;
pub const IRQ: u32 = 21;

#[derive(Clone, Debug)]
pub struct Tim2 {
    /// Registers by offset / 4, 0x00..=0x44 (CNT and PSC live below).
    regs: [u32; 18],
    cnt: u32,
    /// The prescaler counting now (PSC is its preload).
    psc: u32,
    /// When `cnt` was last right: a timer clock edge.
    since: u64,
    /// The timer clock, PCLK1, in Hz; 0 while RCC has it off.
    hz: u64,
}

impl Default for Tim2 {
    fn default() -> Self {
        let mut t = Tim2 {
            regs: [0; 18],
            cnt: 0,
            psc: 0,
            since: 0,
            hz: 0,
        };
        t.regs[(ARR / 4) as usize] = 0xFFFF;
        t
    }
}

impl Tim2 {
    fn r(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn r_mut(&mut self, off: u32) -> &mut u32 {
        &mut self.regs[(off / 4) as usize]
    }

    /// Counting: enabled (CEN) and clocked.
    fn running(&self) -> bool {
        self.r(CR1) & CEN != 0 && self.hz != 0
    }

    /// Core ticks per count. A PCLK1 that doesn't divide TICK_HZ rounds to
    /// the nearest tick (none of our images sets one).
    fn tick(&self) -> u64 {
        (TICK_HZ + self.hz / 2) / self.hz * (self.psc as u64 + 1)
    }

    pub fn hz(&self) -> u64 {
        self.hz
    }

    /// The timer clock changes at `now` (PCLK1, or RCC gating it): the
    /// count so far stands, and the next one starts then.
    pub fn set_clock(&mut self, hz: u64, now: u64) {
        self.sync(now);
        self.hz = hz;
        self.since = now;
    }

    /// Timer clocks from `cnt` to the next wrap, and whether that wrap is
    /// an update (at ARR) or the 16-bit roll-over past it.
    fn to_wrap(&self) -> (u64, bool) {
        let arr = self.r(ARR);
        if self.cnt <= arr {
            ((arr - self.cnt) as u64 + 1, true)
        } else {
            (0x1_0000 - self.cnt as u64, false)
        }
    }

    /// Brings the counter and flags up to `now`.
    pub fn sync(&mut self, now: u64) {
        if !self.running() {
            self.since = now;
            return;
        }
        loop {
            let (clocks, update) = self.to_wrap();
            let at = self.since + clocks * self.tick();
            if at > now {
                let n = (now - self.since) / self.tick();
                self.cnt += n as u32;
                self.since += n * self.tick();
                return;
            }
            self.since = at;
            self.cnt = 0;
            if update {
                self.update_event();
                if self.r(CR1) & OPM != 0 {
                    *self.r_mut(CR1) &= !CEN;
                    self.since = now;
                    return;
                }
            }
        }
    }

    fn update_event(&mut self) {
        *self.r_mut(SR) |= WRAP_FLAGS;
        self.psc = self.regs[(PSC / 4) as usize];
    }

    /// When the counter next wraps, if it's running.
    pub fn next_event(&self) -> u64 {
        if self.running() {
            self.since + self.to_wrap().0 * self.tick()
        } else {
            u64::MAX
        }
    }

    pub fn irq(&self) -> bool {
        self.r(SR) & self.r(DIER) & IRQ_FLAGS != 0
    }

    pub fn read(&mut self, off: u32, now: u64) -> u32 {
        self.sync(now);
        match off {
            CNT => self.cnt,
            EGR => 0,
            o if o % 4 == 0 && (o as usize) < self.regs.len() * 4 => self.r(o),
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64) {
        self.sync(now);
        match off {
            SR => *self.r_mut(SR) &= v, // rc_w0
            EGR => {
                if v & 1 != 0 {
                    self.cnt = 0;
                    self.psc = self.r(PSC);
                    self.since = now;
                    if self.r(CR1) & URS == 0 {
                        *self.r_mut(SR) |= UIF;
                    }
                }
            }
            CNT => self.cnt = v & 0xFFFF,
            CR1 => {
                if self.r(CR1) & CEN == 0 {
                    self.since = now;
                }
                *self.r_mut(CR1) = v;
            }
            PSC | ARR | 0x34..=0x40 => *self.r_mut(off) = v & 0xFFFF,
            o if o % 4 == 0 && (o as usize) < self.regs.len() * 4 => *self.r_mut(o) = v,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = TICK_HZ / 1000;
    /// Core ticks per timer clock at 96 MHz.
    const CLOCK: u64 = TICK_HZ / 96_000_000;

    /// Clocked at 96 MHz, as our clock set-up gives it.
    fn tim() -> Tim2 {
        let mut t = Tim2::default();
        t.set_clock(96_000_000, 0);
        t
    }

    /// akko-rmk's set-up: 10 kHz, 2500 counts, update interrupt.
    fn periodic() -> Tim2 {
        let mut t = tim();
        t.write(CR1, URS, 0);
        t.write(PSC, 9599, 0);
        t.write(ARR, 2499, 0);
        t.write(EGR, 1, 0);
        t.write(SR, 0, 0);
        t.write(DIER, 1, 0);
        t.write(CR1, URS | CEN, 0);
        t
    }

    #[test]
    fn update_every_period() {
        let mut t = periodic();
        assert!(!t.irq(), "URS: UG sets no flag");
        assert_eq!(t.next_event(), 250 * MS);
        assert_eq!(t.read(CNT, 100 * MS), 1000);
        t.sync(250 * MS - 1);
        assert!(!t.irq());
        t.sync(250 * MS);
        assert!(t.irq());
        assert_eq!(t.read(CNT, 250 * MS), 0);
        t.write(SR, !UIF, 250 * MS);
        assert!(!t.irq());
        assert_eq!(t.next_event(), 500 * MS);
    }

    #[test]
    fn sixteen_bits_and_compare_at_wrap_only() {
        let mut t = tim();
        t.write(ARR, 0x1_2345, 0);
        assert_eq!(t.read(ARR, 0), 0x2345);
        t.write(ARR, 0xFFFF, 0);
        t.write(0x34, 1000, 0); // CCR1
        t.write(DIER, 1 << 1, 0); // CC1IE
        t.write(CR1, CEN, 0);
        t.sync(2000 * CLOCK);
        assert!(
            !t.irq(),
            "CNT passed CCR1, but the chip only flags at the wrap"
        );
        t.sync(0x1_0000 * CLOCK);
        assert!(t.irq());
    }

    #[test]
    fn psc_waits_for_an_update() {
        let mut t = tim();
        t.write(CR1, CEN, 0);
        t.write(PSC, 1, 0);
        assert_eq!(t.read(CNT, 10 * CLOCK), 10);
        t.write(EGR, 1, 10 * CLOCK);
        assert_eq!(t.read(CNT, 30 * CLOCK), 10);
    }

    #[test]
    fn it_counts_pclk1_and_stops_without_its_clock() {
        let mut t = Tim2::default();
        t.write(CR1, CEN, 0);
        assert_eq!(t.read(CNT, MS), 0, "no clock from reset");
        assert_eq!(t.next_event(), u64::MAX);
        // 48 MHz: 48 counts a microsecond.
        t.set_clock(48_000_000, MS);
        assert_eq!(t.read(CNT, MS + MS / 1000), 48);
        // Gated at 2 ms: the count holds, then carries on at 96 MHz.
        t.set_clock(0, 2 * MS);
        assert_eq!(t.read(CNT, 3 * MS), 48_000 % 0x1_0000);
        t.set_clock(96_000_000, 3 * MS);
        assert_eq!(t.read(CNT, 3 * MS + 10 * CLOCK), 48_000 + 10);
    }
}
