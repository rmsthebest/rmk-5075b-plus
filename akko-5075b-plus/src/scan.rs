//! Key matrix scan, plus the Win/Mac switch.
//!
//! Polarity as QMK's ROW2COL (and boot-guard's Esc check): each column in
//! turn is driven low, the others are released, and a held key pulls its
//! row (an input with pull-up) low through its diode. QMK switches each
//! column between push-pull output and pulled-up input; here the columns are
//! open-drain outputs with pull-up, which is the same on the wire (low:
//! driven low; high: driver off, pulled up) but selects a column with one
//! BSRR write instead of a full pin reconfiguration. One pass per
//! millisecond, as QMK's matrix task; RMK's own matrix drives the other way
//! and needs every input on EXTI, which this board's pins can't do (lines
//! A6/C6 and A10/B10 clash).
//!
//! Settling: 5 us after selecting a column before reading the rows (QMK
//! waits ~0.25 us), and after releasing a column that had a key down, 30 us
//! for its rows to come back up through their pull-ups (QMK's
//! MATRIX_IO_DELAY, which QMK waits after every column).
//!
//! Debounce: per key, a change counts once the pin has read the same for
//! `DEBOUNCE_MS` passes in a row. Passes are at least 1 ms apart, also after
//! the executor was held up (a Ticker would catch up with back-to-back
//! passes). Same 5 ms as QMK's default DEBOUNCE, though QMK debounces the
//! whole matrix at once (sym_defer_g) and this does it per key.
//!
//! The Win/Mac switch (C15 Win / C14 Mac, active low, pulled up) is read on
//! the same pass. When it moves, and once at start-up, the scan taps the
//! keymap position that sets the matching default layer (keymap::SWITCH_*).

use embassy_time::Timer;
use rmk::event::KeyboardEvent;
use rmk::macros::input_device;
use wb32_hal::gpio::{AnyPin, Input, Level, Mode, Pull};

use crate::keymap::{COL, ROW, SWITCH_MAC, SWITCH_WIN};

const DEBOUNCE_MS: u8 = 5;
const SELECT_SETTLE_US: u32 = 5;
const RELEASE_SETTLE_US: u32 = 30;

#[input_device(publish = KeyboardEvent)]
pub struct Scan {
    rows: [Input; ROW],
    cols: [AnyPin; COL],
    mac: Input,
    /// Debounced state, bit c of word r = key (r, c) down.
    state: [u16; ROW],
    /// Passes the raw reading has differed from `state`, per key.
    count: [[u8; COL]; ROW],
    /// Switch: last reported position (None before the first report), and
    /// the release of the tap still to send.
    mac_mode: Option<bool>,
    tap_release: Option<(u8, u8)>,
}

impl Scan {
    pub fn new(rows: [AnyPin; ROW], cols: [AnyPin; COL], win: AnyPin, mac: AnyPin) -> Self {
        for c in &cols {
            c.set_level(Level::High);
            c.set_mode(Mode { pull: Pull::Up as u8, ..Mode::output_open_drain() });
        }
        // Not read: Mac is enough to tell the two positions apart. Pulled up
        // as in QMK, so it doesn't float.
        core::mem::forget(Input::new(win, Pull::Up));
        Scan {
            rows: rows.map(|p| Input::new(p, Pull::Up)),
            cols,
            mac: Input::new(mac, Pull::Up),
            state: [0; ROW],
            count: [[0; COL]; ROW],
            mac_mode: None,
            tap_release: None,
        }
    }

    /// Reads all keys once: bit c of word r = key (r, c) down.
    fn read(&mut self) -> [u16; ROW] {
        let mut raw = [0; ROW];
        for (c, col) in self.cols.iter().enumerate() {
            col.set_level(Level::Low);
            delay_us(SELECT_SETTLE_US);
            let mut any = false;
            for (r, row) in self.rows.iter().enumerate() {
                if row.is_low() {
                    raw[r] |= 1 << c;
                    any = true;
                }
            }
            col.set_level(Level::High);
            if any {
                delay_us(RELEASE_SETTLE_US);
            }
        }
        raw
    }

    /// The next debounced change, if any, applying it to `state`.
    fn next_change(&mut self, raw: &[u16; ROW]) -> Option<KeyboardEvent> {
        for r in 0..ROW {
            let diff = raw[r] ^ self.state[r];
            for c in 0..COL {
                if diff & (1 << c) == 0 {
                    self.count[r][c] = 0;
                } else if self.count[r][c] + 1 >= DEBOUNCE_MS {
                    self.count[r][c] = 0;
                    self.state[r] ^= 1 << c;
                    return Some(KeyboardEvent::key(r as u8, c as u8, self.state[r] & (1 << c) != 0));
                }
            }
        }
        None
    }

    fn count_up(&mut self, raw: &[u16; ROW]) {
        for r in 0..ROW {
            let diff = raw[r] ^ self.state[r];
            for c in 0..COL {
                if diff & (1 << c) != 0 {
                    self.count[r][c] += 1;
                }
            }
        }
    }

    async fn read_keyboard_event(&mut self) -> KeyboardEvent {
        loop {
            if let Some((r, c)) = self.tap_release.take() {
                return KeyboardEvent::key(r, c, false);
            }
            let mac = self.mac.is_low();
            if self.mac_mode != Some(mac) {
                self.mac_mode = Some(mac);
                let (r, c) = if mac { SWITCH_MAC } else { SWITCH_WIN };
                self.tap_release = Some((r, c));
                return KeyboardEvent::key(r, c, true);
            }
            let raw = self.read();
            if let Some(e) = self.next_change(&raw) {
                return e;
            }
            self.count_up(&raw);
            Timer::after_millis(1).await;
        }
    }
}

fn delay_us(us: u32) {
    cortex_m::asm::delay(wb32_hal::rcc::HCLK_HZ / 1_000_000 * us);
}
