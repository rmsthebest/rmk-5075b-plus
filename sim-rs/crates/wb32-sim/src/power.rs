//! The chip's supply current at 3.3 V, from what the datasheet gives
//! (DS1905020 p.1 and 5.2) and the RM's low-power modes (RM2905025 5.3):
//! what the firmware's clocks, sleeps and flash writes cost the MCU, not
//! the board (LEDs, radio module, NOR flash, charger and regulator aren't
//! in it).
//!
//! Given:
//! - Run: ~140 µA/MHz of HCLK, "peripherals disabled": 140 pC each core
//!   clock awake, at any HCLK. The flash's read current (DS table 5.2-10,
//!   4.7 mA at 48 MHz at most) is taken to be inside that figure.
//! - Stop: ~35 µA. The DS doesn't say which of the RM's four stop modes
//!   (table 5-5) that is; it's taken as the low-power ones the vendor and
//!   the HAL use (LP4 with the SRAMs on).
//! - Oscillators, while enabled and not stopped: MHSI 25 µA, FHSI 55 µA,
//!   LSI 0.25 µA (DS tables 5.2-6 to 5.2-8).
//! - The FMC while busy: a page program 2 mA, an erase 1.5 mA (DS table
//!   5.2-10, maximums; the mass erase is taken as an erase).
//!
//! Not given anywhere, so reported rather than guessed:
//! - Sleep (WFI/WFE without stop): only the CPU's clock stops; the bus,
//!   flash, SysTick and the peripherals run on (RM table 5-2, 5.3.3). The
//!   estimate brackets it, from nothing to run's 140 µA/MHz.
//! - The HSE crystal oscillator and the PLL: the time each is on.
//! - Peripheral clocks (the run figure has them off).

use cortex_m3_emu::TICK_HZ;

/// Run: 140 µA/MHz is 140 pC a core clock.
pub const RUN_PC_PER_CYCLE: f64 = 140.0;
pub const STOP_UA: f64 = 35.0;
pub const MHSI_UA: f64 = 25.0;
pub const FHSI_UA: f64 = 55.0;
pub const LSI_UA: f64 = 0.25;
pub const PROGRAM_UA: f64 = 2_000.0;
pub const ERASE_UA: f64 = 1_500.0;

/// What the core is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Core {
    Awake,
    /// Sleep mode: the CPU's clock off, the rest running.
    Asleep,
    /// Stop mode: every clock off.
    Stopped,
}

/// The oscillators enabled (RM 11.4's enable registers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Oscillators {
    pub mhsi: bool,
    pub fhsi: bool,
    pub lsi: bool,
    pub hse: bool,
    pub pll: bool,
}

/// The chip's state over a stretch of time.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub core: Core,
    pub hclk: u64,
    pub osc: Oscillators,
    /// The FMC's op under way: its window (ticks) and current.
    pub flash: Option<(u64, u64, f64)>,
}

/// Charge drawn since power-on, by what drew it. Subtract an earlier copy
/// (`Meter::since`) for a window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meter {
    /// The time it's charged up to.
    pub at: u64,
    pub awake_ticks: u64,
    pub asleep_ticks: u64,
    pub stopped_ticks: u64,
    /// HCLK cycles with the core awake, and asleep.
    pub awake_cycles: f64,
    pub asleep_cycles: f64,
    /// µC drawn by the oscillators the DS gives a figure for, and the FMC.
    pub osc_uc: f64,
    pub flash_uc: f64,
    /// Ticks the HSE and the PLL were on (not stopped): no DS figure.
    pub hse_ticks: u64,
    pub pll_ticks: u64,
}

fn secs(ticks: u64) -> f64 {
    ticks as f64 / TICK_HZ as f64
}

impl Meter {
    /// Charges from `self.at` to `now` in `s`.
    pub fn charge(&mut self, now: u64, s: State) {
        if now <= self.at {
            return;
        }
        let (from, dt) = (self.at, now - self.at);
        self.at = now;
        let cycles = s.hclk as f64 * secs(dt);
        match s.core {
            Core::Awake => {
                self.awake_ticks += dt;
                self.awake_cycles += cycles;
            }
            Core::Asleep => {
                self.asleep_ticks += dt;
                self.asleep_cycles += cycles;
            }
            Core::Stopped => {
                self.stopped_ticks += dt;
                return;
            }
        }
        let o = s.osc;
        let ua = [(o.mhsi, MHSI_UA), (o.fhsi, FHSI_UA), (o.lsi, LSI_UA)]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, ua)| ua)
            .sum::<f64>();
        self.osc_uc += ua * secs(dt);
        if o.hse {
            self.hse_ticks += dt;
        }
        if o.pll {
            self.pll_ticks += dt;
        }
        if let Some((start, end, ua)) = s.flash {
            let busy = end.min(now).saturating_sub(start.max(from));
            self.flash_uc += ua * secs(busy);
        }
    }

    /// The window from `earlier` to `self`.
    pub fn since(&self, earlier: &Meter) -> Meter {
        Meter {
            at: self.at - earlier.at,
            awake_ticks: self.awake_ticks - earlier.awake_ticks,
            asleep_ticks: self.asleep_ticks - earlier.asleep_ticks,
            stopped_ticks: self.stopped_ticks - earlier.stopped_ticks,
            awake_cycles: self.awake_cycles - earlier.awake_cycles,
            asleep_cycles: self.asleep_cycles - earlier.asleep_cycles,
            osc_uc: self.osc_uc - earlier.osc_uc,
            flash_uc: self.flash_uc - earlier.flash_uc,
            hse_ticks: self.hse_ticks - earlier.hse_ticks,
            pll_ticks: self.pll_ticks - earlier.pll_ticks,
        }
    }

    /// The mean current over the window this is (`since`'s result).
    pub fn estimate(&self) -> Estimate {
        let t = secs(self.at);
        let ua = |uc: f64| uc / t / 1000.0;
        let run_uc = self.awake_cycles * RUN_PC_PER_CYCLE * 1e-6;
        let sleep_uc = self.asleep_cycles * RUN_PC_PER_CYCLE * 1e-6;
        let floor = run_uc + STOP_UA * secs(self.stopped_ticks) + self.osc_uc + self.flash_uc;
        let share = |ticks: u64| secs(ticks) / t;
        Estimate {
            floor_ma: ua(floor),
            sleep_as_run_ma: ua(floor + sleep_uc),
            awake: share(self.awake_ticks),
            asleep: share(self.asleep_ticks),
            stopped: share(self.stopped_ticks),
            asleep_mhz: if self.asleep_ticks == 0 {
                0.0
            } else {
                self.asleep_cycles / secs(self.asleep_ticks) / 1e6
            },
            hse: share(self.hse_ticks),
            pll: share(self.pll_ticks),
        }
    }
}

/// A window's mean current, in mA, as far as the DS goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// With sleep costing nothing: what's certain.
    pub floor_ma: f64,
    /// With sleep costing as much as run.
    pub sleep_as_run_ma: f64,
    /// Shares of the time awake, asleep and stopped.
    pub awake: f64,
    pub asleep: f64,
    pub stopped: f64,
    /// HCLK while asleep, the clock sleep's unknown current scales with.
    pub asleep_mhz: f64,
    /// Shares of the time the HSE and the PLL were on.
    pub hse: f64,
    pub pll: f64,
}

impl std::fmt::Display for Estimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCU {:.3}-{:.3} mA (awake {:.2}%, asleep {:.2}% at {:.0} MHz, stopped {:.2}%; \
             HSE {:.0}%, PLL {:.0}% on)",
            self.floor_ma,
            self.sleep_as_run_ma,
            self.awake * 100.0,
            self.asleep * 100.0,
            self.asleep_mhz,
            self.stopped * 100.0,
            self.hse * 100.0,
            self.pll * 100.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = TICK_HZ;

    fn state(core: Core, hclk: u64) -> State {
        State {
            core,
            hclk,
            osc: Oscillators::default(),
            flash: None,
        }
    }

    #[test]
    fn run_is_140_ua_per_mhz_and_stop_35_ua() {
        let mut m = Meter::default();
        m.charge(S, state(Core::Awake, 96_000_000));
        let e = m.since(&Meter::default()).estimate();
        assert!((e.floor_ma - 13.44).abs() < 1e-9, "{e}");
        assert_eq!(e.floor_ma, e.sleep_as_run_ma);

        let mut m = Meter::default();
        m.charge(S, state(Core::Stopped, 96_000_000));
        let e = m.since(&Meter::default()).estimate();
        assert!((e.floor_ma - 0.035).abs() < 1e-12, "{e}");
        assert_eq!(e.stopped, 1.0);
    }

    #[test]
    fn sleep_is_bracketed_by_nothing_and_run() {
        let mut m = Meter::default();
        m.charge(S / 4, state(Core::Awake, 8_000_000));
        m.charge(S, state(Core::Asleep, 8_000_000));
        let e = m.since(&Meter::default()).estimate();
        assert!((e.floor_ma - 0.28).abs() < 1e-9, "{e}");
        assert!((e.sleep_as_run_ma - 1.12).abs() < 1e-9, "{e}");
        assert_eq!((e.awake, e.asleep, e.asleep_mhz), (0.25, 0.75, 8.0));
    }

    #[test]
    fn oscillators_count_unless_stopped() {
        let osc = Oscillators {
            mhsi: true,
            fhsi: true,
            hse: true,
            pll: true,
            lsi: false,
        };
        let mut m = Meter::default();
        m.charge(
            S / 2,
            State {
                osc,
                ..state(Core::Asleep, 0)
            },
        );
        m.charge(
            S,
            State {
                osc,
                ..state(Core::Stopped, 0)
            },
        );
        let e = m.since(&Meter::default()).estimate();
        assert!((e.floor_ma - (0.040 + 0.0175)).abs() < 1e-12, "{e}");
        assert_eq!((e.hse, e.pll), (0.5, 0.5));
    }

    #[test]
    fn the_fmc_draws_only_inside_its_op() {
        let mut m = Meter::default();
        let busy = State {
            flash: Some((S / 4, S / 2, ERASE_UA)),
            ..state(Core::Awake, 0)
        };
        m.charge(S / 3, busy);
        m.charge(S, busy);
        let e = m.since(&Meter::default()).estimate();
        assert!((e.floor_ma - 0.375).abs() < 1e-9, "{e}");
    }

    #[test]
    fn a_window_subtracts_what_came_before() {
        let mut m = Meter::default();
        m.charge(S, state(Core::Awake, 96_000_000));
        let start = m;
        m.charge(2 * S, state(Core::Stopped, 0));
        let e = m.since(&start).estimate();
        assert!((e.floor_ma - 0.035).abs() < 1e-12, "{e}");
    }
}
