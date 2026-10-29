//! The board's small parts on GPIO: the knob, the Win/Mac switch and the
//! battery charger. Each drives pins from outside the chip
//! (`Gpio::driven`), as the parts do on the board; a pin a part leaves
//! open reads as the chip's pull.
//!
//! - The knob (A on B7, B on B6) is a quadrature encoder whose contacts
//!   close to ground; at a detent both are open. Clockwise, A closes first
//!   (QMK's decoding of this knob): A, B, then A opens, then B.
//! - The Win/Mac switch holds exactly one of C14 (Mac) and C15 (Win) low.
//! - The charger (5075b_plus config.h: BAT_FULL_PIN A15, pulled up) pulls
//!   A15 low while charging, which is while the cable is in (A7) and the
//!   battery isn't full. The level rises at `rate` while charging and falls
//!   at `drain` on the battery, and the radio module reports it.

use cortex_m3_emu::TICK_HZ;
use wb32_sim::gpio::{A, B, C, Gpio, Pin};

pub const KNOB_A: Pin = (B, 7);
pub const KNOB_B: Pin = (B, 6);
pub const MAC: Pin = (C, 14);
pub const WIN: Pin = (C, 15);
/// The cable sense: high with a cable in (VBUS through a divider).
pub const CABLE: Pin = (A, 7);
/// The charger's status: low while charging.
pub const CHARGING: Pin = (A, 15);

/// (A closed, B closed) by quadrature phase, from the detent clockwise.
const PHASES: [(bool, bool); 4] = [(false, false), (true, false), (true, true), (false, true)];

#[derive(Clone, Copy, Debug, Default)]
pub struct Knob {
    phase: usize,
}

impl Knob {
    /// One quadrature edge; the pin that changed and its new level (closed:
    /// low; open: released to the pull).
    pub fn edge(&mut self, clockwise: bool) -> (Pin, Option<bool>) {
        let before = PHASES[self.phase];
        self.phase = (self.phase + if clockwise { 1 } else { 3 }) % 4;
        let after = PHASES[self.phase];
        let level = |closed: bool| if closed { Some(false) } else { None };
        if before.0 != after.0 {
            (KNOB_A, level(after.0))
        } else {
            (KNOB_B, level(after.1))
        }
    }

    pub fn at_detent(&self) -> bool {
        self.phase == 0
    }
}

/// The switch's levels: (C14, C15).
pub fn mac_switch(gpio: &mut Gpio, mac: bool) {
    gpio.driven.remove(&MAC);
    gpio.driven.remove(&WIN);
    gpio.driven.insert(if mac { MAC } else { WIN }, false);
    gpio.update();
}

#[derive(Clone, Copy, Debug)]
pub struct Charger {
    /// Percent.
    pub level: f64,
    /// Percent per second while charging, and while on the battery.
    pub rate: f64,
    pub drain: f64,
    /// When `level` was last brought up to date (core ticks).
    at: u64,
}

impl Default for Charger {
    fn default() -> Self {
        Charger {
            level: 100.0,
            // About 2 h from flat, a 4000 mAh cell at 0.5 C (an assumption:
            // the cell and the charger are under the case).
            rate: 100.0 / 7200.0,
            drain: 0.0,
            at: 0,
        }
    }
}

impl Charger {
    /// Brings the level up to `now`, and A15 with it.
    pub fn advance(&mut self, now: u64, plugged: bool, gpio: &mut Gpio) {
        let dt = now.saturating_sub(self.at) as f64 / TICK_HZ as f64;
        self.at = now;
        let charging = plugged && self.level < 100.0;
        self.level = if charging {
            (self.level + self.rate * dt).min(100.0)
        } else if !plugged {
            (self.level - self.drain * dt).max(0.0)
        } else {
            self.level
        };
        let low = plugged && self.level < 100.0;
        if gpio.driven.get(&CHARGING).copied() != low.then_some(false) {
            if low {
                gpio.driven.insert(CHARGING, false);
            } else {
                gpio.driven.remove(&CHARGING);
            }
            gpio.update();
        }
    }

    /// The level the radio module reports, in percent.
    pub fn percent(&self) -> u8 {
        self.level.round() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detent_is_four_edges_and_a_leads_clockwise() {
        let mut k = Knob::default();
        let cw: Vec<_> = (0..4).map(|_| k.edge(true)).collect();
        assert_eq!(
            cw,
            [
                (KNOB_A, Some(false)),
                (KNOB_B, Some(false)),
                (KNOB_A, None),
                (KNOB_B, None)
            ]
        );
        assert!(k.at_detent());
        let ccw: Vec<_> = (0..4).map(|_| k.edge(false)).collect();
        assert_eq!(
            ccw,
            [
                (KNOB_B, Some(false)),
                (KNOB_A, Some(false)),
                (KNOB_B, None),
                (KNOB_A, None)
            ]
        );
    }

    #[test]
    fn the_charger_pulls_a15_low_until_full() {
        let mut g = Gpio::default();
        let mut c = Charger {
            level: 99.0,
            rate: 1.0,
            ..Charger::default()
        };
        c.advance(0, true, &mut g);
        assert_eq!(g.driven.get(&CHARGING), Some(&false));
        c.advance(TICK_HZ / 2, true, &mut g);
        assert_eq!((c.level, g.driven.get(&CHARGING)), (99.5, Some(&false)));
        c.advance(2 * TICK_HZ, true, &mut g);
        assert_eq!((c.level, g.driven.get(&CHARGING)), (100.0, None));
        c.level = 50.0;
        c.advance(3 * TICK_HZ, false, &mut g);
        assert_eq!((c.level, g.driven.get(&CHARGING)), (50.0, None), "no cable");
    }
}
