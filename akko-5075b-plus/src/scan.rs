//! Key matrix scan, plus the Win/Mac switch.
//!
//! Polarity as QMK's ROW2COL (and boot-guard's Esc check): each column in
//! turn is driven low, the others are released, and a held key pulls its
//! row (an input with pull-up) low through its diode. As QMK, a selected
//! column is a push-pull output (its ODR bit stays low) and a released one a
//! pulled-up input. Open-drain outputs with pull-up typed nothing on the
//! board, for a reason not found yet, though the sim and the vendor's GPIO
//! code say they should work. One pass per
//! millisecond, as QMK's matrix task; RMK's own matrix drives the other way
//! and needs every input on EXTI, which this board's pins can't do (lines
//! A6/C6 and A10/B10 clash).
//!
//! Settling: 5 us after selecting a column before reading the rows (QMK
//! waits ~0.25 us), and after releasing a column that had a key down, 30 us
//! for its rows to come back up through their pull-ups (QMK's
//! MATRIX_IO_DELAY, which QMK waits after every column).
//!
//! Debounce: per key, in milliseconds (the time since the previous pass is
//! added to every key whose reading differs from its state, and a reading
//! that agrees starts it over). A release counts once the key has read up
//! for `DEBOUNCE_MS`. A press counts at its first reading (QMK's
//! asym_eager_defer_pk), unless `defer_press` is set, when it also waits
//! `DEBOUNCE_MS` (QMK's default 5 ms, though QMK debounces the whole matrix
//! at once). The setting is kept in the lighting record (src/lighting.rs) and
//! set over Vial channel 0xD6 (src/main.rs), for a switch that chatters.
//! Passes are one SysTick tick (1 ms) apart:
//! each waits for the millisecond after the one it started in, which the
//! time base wakes at that tick, however long the pass took (a fixed 1 ms
//! wait would land on the tick after, every 2 ms; on the board a 0.5 ms
//! wait did too while a key was down, as the pass then takes longer). After
//! the executor was held up, the next pass comes at once, with no catching
//! up (as a Ticker would, with back-to-back passes). Several changes found
//! by one pass are reported one per call without scanning again, and count
//! no time towards anything.
//!
//! Live diagnostics, for the bench while typing fails on the board: VIA's
//! id_custom_get_value (0x08) on channel 0xD1 (src/main.rs) answers with the
//! scan's counters (see `fill_diag` for the layout). Counters are totals
//! since boot except the two marked "since the last query".
//!
//! The Win/Mac switch (C15 Win / C14 Mac, active low, pulled up) is read on
//! the same pass. When it moves, and once at start-up, the scan taps the
//! keymap position that sets the matching default layer (keymap::SWITCH_*).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Instant, Timer};
use rmk::event::KeyboardEvent;
use rmk::macros::input_device;
use wb32_hal::gpio::{AnyPin, Input, Level, Mode, Pull};

use crate::keymap::{COL, ROW, SWITCH_MAC, SWITCH_WIN};

const DEBOUNCE_MS: u8 = 5;

/// Presses wait out the debounce too (see the top of this file).
static DEFER_PRESS: AtomicBool = AtomicBool::new(false);

pub fn defer_press() -> bool {
    DEFER_PRESS.load(Ordering::Relaxed)
}

pub fn set_defer_press(on: bool) {
    DEFER_PRESS.store(on, Ordering::Relaxed);
}
const SELECT_SETTLE_US: u32 = 5;
const RELEASE_SETTLE_US: u32 = 30;
/// Column modes, as QMK's select_col/unselect_col (ODR stays low).
const SELECTED: Mode = Mode::output();
const RELEASED: Mode = Mode::input(Pull::Up);

struct Diag {
    passes: u32,
    /// Passes where any key read down.
    down_passes: u32,
    /// Key readings that differ from the pass before.
    flips: u32,
    /// Debounced changes sent to RMK.
    events: u32,
    /// Since the last query: longest time between two passes, and the OR of
    /// every pass's reading.
    max_gap_us: u32,
    seen: [u16; ROW],
    /// The previous pass's reading.
    raw: [u16; ROW],
    last_pass: Option<Instant>,
}

static DIAG: Mutex<CriticalSectionRawMutex, RefCell<Diag>> = Mutex::new(RefCell::new(Diag {
    passes: 0,
    down_passes: 0,
    flips: 0,
    events: 0,
    max_gap_us: 0,
    seen: [0; ROW],
    raw: [0; ROW],
    last_pass: None,
}));

/// Answers the diagnostics request (`buf` = the 32-byte reply, pre-filled
/// with the request: 08 d1 ...):
/// - 2..6 passes, 6..10 passes with a key down, 10..14 flips, 14..18 events
///   (u32 LE each), 18..20 longest gap between passes in 0.1 ms (u16 LE,
///   since the last query);
/// - the OR of every reading since the last query, per row r: byte 20 + r
///   holds columns 0-7 (bit c), byte 26 + r columns 8-14 (bit c - 8).
pub fn fill_diag(buf: &mut [u8]) {
    if buf.len() < 32 {
        return;
    }
    DIAG.lock(|d| {
        let mut d = d.borrow_mut();
        buf[2..6].copy_from_slice(&d.passes.to_le_bytes());
        buf[6..10].copy_from_slice(&d.down_passes.to_le_bytes());
        buf[10..14].copy_from_slice(&d.flips.to_le_bytes());
        buf[14..18].copy_from_slice(&d.events.to_le_bytes());
        let gap = (d.max_gap_us / 100).min(u16::MAX as u32) as u16;
        buf[18..20].copy_from_slice(&gap.to_le_bytes());
        for r in 0..ROW {
            buf[20 + r] = d.seen[r] as u8;
            buf[26 + r] = (d.seen[r] >> 8) as u8;
        }
        d.max_gap_us = 0;
        d.seen = [0; ROW];
    });
}

fn diag_pass(raw: &[u16; ROW]) {
    let now = Instant::now();
    DIAG.lock(|d| {
        let mut d = d.borrow_mut();
        d.passes = d.passes.wrapping_add(1);
        if raw.iter().any(|&w| w != 0) {
            d.down_passes = d.down_passes.wrapping_add(1);
        }
        let mut flips = 0;
        for r in 0..ROW {
            flips += (raw[r] ^ d.raw[r]).count_ones();
            d.seen[r] |= raw[r];
        }
        d.flips = d.flips.wrapping_add(flips);
        d.raw = *raw;
        // Never panics, even if time ran backwards.
        if let Some(gap) = d.last_pass.and_then(|last| now.checked_duration_since(last)) {
            d.max_gap_us = d.max_gap_us.max(gap.as_micros().min(u32::MAX as u64) as u32);
        }
        d.last_pass = Some(now);
    });
}

fn diag_event() {
    DIAG.lock(|d| {
        let mut d = d.borrow_mut();
        d.events = d.events.wrapping_add(1);
    });
}

#[input_device(publish = KeyboardEvent)]
pub struct Scan {
    rows: [Input; ROW],
    cols: [AnyPin; COL],
    mac: Input,
    /// Debounced state, bit c of word r = key (r, c) down.
    state: [u16; ROW],
    /// The last pass's reading, and when it was (ms).
    raw: [u16; ROW],
    last_pass: Option<u64>,
    /// Milliseconds the reading has differed from `state`, per key
    /// (saturating).
    count: [[u8; COL]; ROW],
    /// Switch: last reported position (None before the first report), and
    /// the release of the tap still to send.
    mac_mode: Option<bool>,
    tap_release: Option<(u8, u8)>,
}

impl Scan {
    pub fn new(rows: [AnyPin; ROW], cols: [AnyPin; COL], win: AnyPin, mac: AnyPin) -> Self {
        for c in &cols {
            c.set_level(Level::Low);
            c.set_mode(RELEASED);
        }
        // Not read: Mac is enough to tell the two positions apart. Pulled up
        // as in QMK, so it doesn't float.
        core::mem::forget(Input::new(win, Pull::Up));
        Scan {
            rows: rows.map(|p| Input::new(p, Pull::Up)),
            cols,
            mac: Input::new(mac, Pull::Up),
            state: [0; ROW],
            raw: [0; ROW],
            last_pass: None,
            count: [[0; COL]; ROW],
            mac_mode: None,
            tap_release: None,
        }
    }

    /// Reads all keys once: bit c of word r = key (r, c) down.
    fn read(&mut self) -> [u16; ROW] {
        let mut raw = [0; ROW];
        for (c, col) in self.cols.iter().enumerate() {
            col.set_mode(SELECTED);
            delay_us(SELECT_SETTLE_US);
            let mut any = false;
            for (r, row) in self.rows.iter().enumerate() {
                if row.is_low() {
                    raw[r] |= 1 << c;
                    any = true;
                }
            }
            col.set_mode(RELEASED);
            if any {
                delay_us(RELEASE_SETTLE_US);
            }
        }
        raw
    }

    /// The next debounced change in the last pass's reading, if any,
    /// applying it to `state`.
    fn next_change(&mut self) -> Option<KeyboardEvent> {
        let eager = !defer_press();
        for r in 0..ROW {
            let diff = self.raw[r] ^ self.state[r];
            if diff == 0 {
                continue;
            }
            for c in 0..COL {
                let bit = 1 << c;
                if diff & bit == 0 {
                    continue;
                }
                let press = self.raw[r] & bit != 0;
                if (press && eager) || self.count[r][c] >= DEBOUNCE_MS {
                    self.count[r][c] = 0;
                    self.state[r] ^= bit;
                    diag_event();
                    return Some(KeyboardEvent::key(r as u8, c as u8, press));
                }
            }
        }
        None
    }

    /// Takes a pass's reading made at `now_ms`: each key that still differs
    /// from its state adds the time since the last pass, any other starts
    /// over.
    fn take_reading(&mut self, raw: [u16; ROW], now_ms: u64) {
        // The first pass counts as one tick.
        let dt = self.last_pass.map_or(1, |t| now_ms.saturating_sub(t)).min(u8::MAX as u64) as u8;
        self.last_pass = Some(now_ms);
        self.raw = raw;
        for r in 0..ROW {
            let diff = raw[r] ^ self.state[r];
            for c in 0..COL {
                self.count[r][c] = if diff & (1 << c) != 0 { self.count[r][c].saturating_add(dt) } else { 0 };
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
            if let Some(e) = self.next_change() {
                return e;
            }
            // The next 1 ms tick (see the top of this file).
            if let Some(last) = self.last_pass {
                Timer::at(Instant::from_millis(last + 1)).await;
            }
            let now = Instant::now().as_millis();
            let raw = self.read();
            diag_pass(&raw);
            self.take_reading(raw, now);
        }
    }
}

fn delay_us(us: u32) {
    cortex_m::asm::delay(wb32_hal::rcc::HCLK_HZ / 1_000_000 * us);
}
