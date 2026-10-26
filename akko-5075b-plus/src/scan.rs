//! Key matrix scan, plus the Win/Mac switch.
//!
//! Polarity as QMK's ROW2COL (and boot-guard's Esc check): each column in
//! turn is driven low, the others float as pulled-up inputs, and a held key
//! pulls its row (an input with pull-up) low through its diode. One pass per
//! millisecond, as QMK's matrix task; RMK's own matrix drives the other way
//! and needs every input on EXTI, which this board's pins can't do (lines
//! A6/C6 and A10/B10 clash).
//!
//! Debounce: per key, a change counts once the pin has read the same for
//! `DEBOUNCE_MS` passes in a row (QMK's sym_defer_pk, 5 ms).
//!
//! The Win/Mac switch (C15 Win / C14 Mac, active low, pulled up) is read on
//! the same pass. When it moves, and once at start-up, the scan taps the
//! keymap position that sets the matching default layer (keymap::SWITCH_*).

use embassy_time::{Duration, Ticker};
use rmk::event::KeyboardEvent;
use rmk::macros::input_device;
use wb32_hal::gpio::{AnyPin, Input, Level, Mode, Pull};

use crate::keymap::{COL, ROW, SWITCH_MAC, SWITCH_WIN};

const DEBOUNCE_MS: u8 = 5;

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
    ticker: Ticker,
}

impl Scan {
    pub fn new(rows: [AnyPin; ROW], cols: [AnyPin; COL], win: AnyPin, mac: AnyPin) -> Self {
        for c in &cols {
            c.set_mode(Mode::input(Pull::Up));
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
            ticker: Ticker::every(Duration::from_millis(1)),
        }
    }

    /// Reads all keys once: bit c of word r = key (r, c) down.
    fn read(&mut self) -> [u16; ROW] {
        let mut raw = [0; ROW];
        for (c, col) in self.cols.iter().enumerate() {
            col.set_level(Level::Low);
            col.set_mode(Mode::output());
            // Let the row lines settle (QMK waits 30 us; a few us is plenty
            // for the pull-ups against these short traces).
            cortex_m::asm::delay(wb32_hal::rcc::HCLK_HZ / 1_000_000 * 5);
            for (r, row) in self.rows.iter().enumerate() {
                if row.is_low() {
                    raw[r] |= 1 << c;
                }
            }
            col.set_mode(Mode::input(Pull::Up));
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
            self.ticker.next().await;
        }
    }
}
