//! Sleep (M7b): "light sleep", as the vendor's lowpower.c decides when.
//! What saves power is what stops:
//! - the lighting: the LED chain's supply (A8) and the boost (A9) off, and
//!   the aux, Win-lock and Caps Lock LEDs off (src/lighting.rs, `CapsPin`);
//! - the scan: parked on the rows' EXTI lines with every column driven low,
//!   for a change of any row to wake on (src/scan.rs); no 1 kHz scan;
//! - the knob: it waits for an edge, as always (src/knob.rs);
//! - after a while on the battery, the clocks: stop mode (below).
//!
//! When: only while a wireless device is selected (QMK never sleeps the MCU
//! on USB; there the lighting goes off 7 s after the host suspends, see
//! src/lighting.rs):
//! - no key or knob input for the sleep timeout (the setting below);
//! - the link down (DISCONNECTED) for 30 s, or 2 min after a pairing
//!   key's hold; pairing (PAIRING) for 2 min. The module is told first
//!   (devctrl USB, as the vendor's wls.c). Not with the setting "never":
//!   unlike the vendor's, never means never. As the vendor's, typing
//!   doesn't put this off, but a device key restarts it (`link_kick`);
//! - the host suspending (the module's host state frame, `host_state`);
//! - the battery at 0% for a minute (the lighting's `shutdown_requested`),
//!   the cable out: "lower sleep", which keys and the knob don't wake; the
//!   columns stay released. Asleep for the host or idle, it moves to lower
//!   sleep; waking drops a request that came meanwhile.
//!
//! Going to sleep idle with the link up, the keys are released to the host
//! (the vendor's clear_keyboard; not when the host suspends, where a report
//! might wake it). Any sleep ends a pairing key's longer link timeout
//! (hs_pairing_flag, cleared in the vendor's stop_hook_pre).
//!
//! Waking: a key, a knob edge, the cable going in (A7, polled every
//! 250 ms), or the host resuming (whatever put it to sleep, as the vendor's
//! lowpower.c, but the flat battery). The radio re-selects the device (as
//! the vendor's suspend_wakeup_init_kb), and the key that woke it is read
//! and sent as usual.
//!
//! The sleep timeout is the vendor's setting (`record::SLEEP_*`: never, 1,
//! 3, 5, 10, 20, 30 minutes, default 2), set over Vial channel 0xD7
//! (tools/sleep.py) and saved with the lighting record.
//!
//! Stop mode (M7c, src/stop.rs): asleep for STOP_AFTER_MS on the battery
//! (the cable out), the chip stops its clocks until a key, the knob, the
//! cable or the radio module's UART wakes it. A UART wake stays asleep: the
//! radio reads the frame, and it stops again (as the vendor's). Time
//! (SysTick) stands still while stopped. When (`StopMode`, Vial channel
//! 0xD9, tools/sleep.py stop; not saved):
//! - `Link`, from every reset: only with the host gone, so nothing the
//!   module would say matters until a key. A frame that wakes a stop is
//!   lost whole (the clocks come back after it), which a suspended host's
//!   resume can't afford; with the host gone, a key wakes it and selects
//!   the device again, and the module announces the link afresh. Gone is
//!   asleep for the link (down 30 s; the module told to sleep) or the flat
//!   battery, or asleep for anything else and the module DISCONNECTED for
//!   RECONNECT_TIMEOUT_MS since (the computer shut down after the idle
//!   timeout);
//! - `Always`: whenever asleep (the vendor's);
//! - `Off`.

use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use akko_lighting::board::{MD_STATE_DISCONNECTED, MD_STATE_PAIRING};
use akko_lighting::record;
use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use wb32_hal::gpio::{AnyPin, Input, Output, Port, Pull};
use wb32_hal::uart;

use crate::radio;
use crate::stop;

/// HS_LBACK_TIMEOUT and HS_PAIR_TIMEOUT (wls.h).
const RECONNECT_TIMEOUT_MS: u32 = 30_000;
const PAIR_TIMEOUT_MS: u32 = 120_000;
/// How often the task looks asleep (the cable, and whether to stop), and
/// at least how often awake (the link's timeouts; the idle timeout is
/// waited for exactly). Pokes wake it at once.
const TICK_MS: u32 = 250;
const AWAKE_TICK_MS: u32 = 1_000;
/// The vendor's wait after telling the module (wls.c, wait_ms(200)).
const MODULE_SLEEP_WAIT_MS: u64 = 200;
/// Asleep this long (the lights off, the columns driven) before stopping.
const STOP_AFTER_MS: u32 = 1_000;
/// After a stop, awake this long first (the vendor's LPWR_STOP_DELAY): a
/// UART wake's frame comes in meanwhile.
const STOP_DELAY_MS: u32 = 200;

static SETTING: AtomicU8 = AtomicU8::new(record::SLEEP_DEFAULT);
static SLEEPING: AtomicBool = AtomicBool::new(false);
/// Asleep for the flat battery: only the cable wakes.
static LOWER: AtomicBool = AtomicBool::new(false);
/// The last key or knob input, ms.
static LAST_INPUT: AtomicU32 = AtomicU32::new(0);
/// Asked to sleep now: the host suspended, or the battery is flat.
static HOST_SUSPENDED: AtomicBool = AtomicBool::new(false);
/// The host resumed since it last slept (the module's frame).
static RESUMED: AtomicBool = AtomicBool::new(false);
/// A device key: the link timers start again.
static LINK_KICK: AtomicBool = AtomicBool::new(false);
/// USB is configured and not suspended (QMK's USB_DRIVER.state ==
/// USB_ACTIVE).
static USB_ACTIVE: AtomicBool = AtomicBool::new(false);
/// The lighting's flat-battery request, until the power task takes it:
/// awake, or asleep for the host or idle (`flat_while_asleep`). It counts
/// only with the cable out, and waking clears it.
static FLAT: AtomicBool = AtomicBool::new(false);
/// Wakes the task: input while asleep, the host's state, a flat battery.
static POKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// When to stop (`StopMode`, Vial 0xD9; `Link` at reset).
static STOP_MODE: AtomicU8 = AtomicU8::new(StopMode::Link as u8);
/// Stop's diagnostic, with its variant bits (`stop::VARIANT_*`); 0xFF off.
static STOP_DIAG: AtomicU8 = AtomicU8::new(DIAG_OFF);
const DIAG_OFF: u8 = 0xFF;
/// Stops so far, and those a key, the knob or the cable ended (the rest:
/// the UART, or nothing that was armed).
static STOPS: AtomicU32 = AtomicU32::new(0);
static STOP_WAKES: AtomicU32 = AtomicU32::new(0);
/// Tries that didn't stop: an interrupt was pending.
static STOP_ABORTS: AtomicU32 = AtomicU32::new(0);

/// When asleep stops (see the top of this file). The values are Vial
/// 0xD9's answer byte 3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum StopMode {
    Off = 0,
    Always = 1,
    Link = 2,
}

pub fn stop_mode() -> StopMode {
    match STOP_MODE.load(Ordering::Relaxed) {
        0 => StopMode::Off,
        1 => StopMode::Always,
        _ => StopMode::Link,
    }
}

pub fn set_stop_mode(mode: StopMode) {
    STOP_MODE.store(mode as u8, Ordering::Relaxed);
    STOP_DIAG.store(DIAG_OFF, Ordering::Relaxed);
}

/// Stop mode on, with its progress on the LEDs and the variant bits
/// `variant` (`stop::stop`'s `diag`; unknown bits ignored).
pub fn set_stop_diag(variant: u8) {
    STOP_MODE.store(StopMode::Always as u8, Ordering::Relaxed);
    STOP_DIAG.store(variant & stop::VARIANTS, Ordering::Relaxed);
}

/// The diagnostic's variant bits, if it's on.
pub fn stop_diag() -> Option<u8> {
    Some(STOP_DIAG.load(Ordering::Relaxed)).filter(|&v| v != DIAG_OFF)
}

/// (stops, those ended by a key, the knob or the cable, tries that didn't
/// stop).
pub fn stop_counts() -> (u32, u32, u32) {
    (
        STOPS.load(Ordering::Relaxed),
        STOP_WAKES.load(Ordering::Relaxed),
        STOP_ABORTS.load(Ordering::Relaxed),
    )
}

/// The sleep timeout, `record::SLEEP_*`.
pub fn setting() -> u8 {
    SETTING.load(Ordering::Relaxed)
}

/// Sets the sleep timeout (from the record at start-up, or Vial); out of
/// range is ignored.
pub fn set_setting(index: u8) {
    if index <= record::SLEEP_DEFAULT {
        SETTING.store(index, Ordering::Relaxed);
    }
}

/// Asleep: the lights, the scan and the knob's poll are off.
pub fn sleeping() -> bool {
    SLEEPING.load(Ordering::Relaxed)
}

/// Asleep for the flat battery: keys don't wake it.
pub fn lower() -> bool {
    LOWER.load(Ordering::Relaxed)
}

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

/// A key or knob input: restarts the idle time, and wakes the keyboard
/// unless it's asleep for the flat battery.
pub fn input() {
    LAST_INPUT.store(now_ms(), Ordering::Relaxed);
    if sleeping() && !lower() {
        SLEEPING.store(false, Ordering::Relaxed);
        POKE.signal(());
    }
}

/// The module's host state frame: the host suspended (false) or resumed.
/// Ignored while USB is selected: it isn't about the host the keys go to
/// (the vendor clears it on USB), and a stale suspend would put it to sleep
/// on the switch to wireless.
pub fn host_state(resume: bool) {
    if !radio::wireless() {
        return;
    }
    HOST_SUSPENDED.store(!resume, Ordering::Relaxed);
    if resume {
        RESUMED.store(true, Ordering::Relaxed);
    }
    POKE.signal(());
}

/// A device key (BT1-3, 2.4G, USB) was pressed: the link timeouts start
/// again (the vendor's hs_rgb_blink_set_timer in WLS_KEYCODE_WAKEUP).
pub fn link_kick() {
    LINK_KICK.store(true, Ordering::Relaxed);
    POKE.signal(());
}

/// USB's state (RMK's): configured or not. On USB the Caps Lock LED is
/// off unless it is, as the vendor's housekeeping.
pub fn set_usb_active(active: bool) {
    USB_ACTIVE.store(active, Ordering::Relaxed);
    refresh_caps();
}

/// The battery sat at 0% for a minute.
pub fn flat_battery() {
    FLAT.store(true, Ordering::Relaxed);
    POKE.signal(());
}

/// Why it went to sleep.
#[derive(Clone, Copy, PartialEq)]
enum Why {
    Idle,
    Link,
    Host,
    Flat,
}

pub struct Power {
    cable: Input,
    /// The module's state, and when it was entered (ms).
    state: u8,
    state_since: u32,
    cable_was: bool,
    /// Why it's asleep (None: awake).
    asleep: Option<Why>,
    /// When it went to sleep, and when it last came out of a stop (ms).
    asleep_at: u32,
    stopped_at: u32,
}

impl Power {
    pub fn new() -> Self {
        // A7, the cable sense; also read by the lighting and the radio.
        let cable = Input::new(unsafe { AnyPin::steal(Port::A, 7) }, Pull::None);
        let now = now_ms();
        LAST_INPUT.store(now, Ordering::Relaxed);
        let cable_was = cable.is_high();
        Power {
            cable,
            state: 0,
            state_since: now,
            cable_was,
            asleep: None,
            asleep_at: now,
            stopped_at: now,
        }
    }

    /// Follows the module's state: since when it's been in it.
    fn track_state(&mut self, now: u32) {
        let state = radio::module_state();
        if state != self.state {
            self.state = state;
            self.state_since = now;
        }
    }

    /// Whether to sleep now, and why.
    fn due(&mut self, now: u32) -> Option<Why> {
        self.track_state(now);
        if LINK_KICK.swap(false, Ordering::Relaxed) {
            self.state_since = now;
        }
        let last = LAST_INPUT.load(Ordering::Relaxed);
        if FLAT.swap(false, Ordering::Relaxed) && radio::wireless() && self.cable.is_low() {
            return Some(Why::Flat);
        }
        if !radio::wireless() {
            return None;
        }
        if HOST_SUSPENDED.load(Ordering::Relaxed) {
            return Some(Why::Host);
        }
        let timeout = record::sleep_ms(setting());
        if timeout == 0 {
            return None;
        }
        if now.wrapping_sub(last) >= timeout {
            return Some(Why::Idle);
        }
        let link = match self.state {
            MD_STATE_DISCONNECTED if radio::pairing() => PAIR_TIMEOUT_MS,
            MD_STATE_DISCONNECTED => RECONNECT_TIMEOUT_MS,
            MD_STATE_PAIRING => PAIR_TIMEOUT_MS,
            _ => return None,
        };
        (now.wrapping_sub(self.state_since) >= link && !crate::lighting::recording())
            .then_some(Why::Link)
    }

    /// Awake: ms until the idle timeout (`AWAKE_TICK_MS` if none).
    fn idle_left(&self, now: u32) -> u32 {
        let timeout = record::sleep_ms(setting());
        if timeout == 0 || !radio::wireless() {
            return AWAKE_TICK_MS;
        }
        // One past it, so `due` sees it reached.
        (timeout + 1).saturating_sub(now.wrapping_sub(LAST_INPUT.load(Ordering::Relaxed)))
    }

    async fn sleep(&mut self, why: Why) {
        if why == Why::Idle {
            radio::release_keys();
        }
        radio::end_pairing();
        RESUMED.store(false, Ordering::Relaxed);
        if matches!(why, Why::Link | Why::Flat) {
            radio::module_sleep();
            Timer::after_millis(MODULE_SLEEP_WAIT_MS).await;
        }
        LOWER.store(why == Why::Flat, Ordering::Relaxed);
        self.cable_was = self.cable.is_high();
        self.asleep = Some(why);
        self.asleep_at = now_ms();
        SLEEPING.store(true, Ordering::Relaxed);
        crate::scan::poke();
        refresh_caps();
    }

    /// Asleep for the host or idle, the battery went flat: the flat
    /// battery's sleep instead (the module told, keys don't wake it).
    fn flat_while_asleep(&self) -> bool {
        matches!(self.asleep, Some(Why::Host | Why::Idle))
            && FLAT.swap(false, Ordering::Relaxed)
            && self.cable.is_low()
    }

    /// Asleep: true once something woke it.
    fn woken(&mut self) -> bool {
        let cable = self.cable.is_high();
        let plugged = cable && !self.cable_was;
        self.cable_was = cable;
        // Not the flat battery's: only the cable ends that.
        let resumed = (RESUMED.swap(false, Ordering::Relaxed)
            || self.asleep == Some(Why::Host) && !HOST_SUSPENDED.load(Ordering::Relaxed))
            && self.asleep != Some(Why::Flat);
        if plugged || resumed {
            SLEEPING.store(false, Ordering::Relaxed);
        }
        !sleeping()
    }

    /// Asleep: whether the host is gone (`StopMode::Link`).
    fn host_gone(&mut self, now: u32) -> bool {
        self.track_state(now);
        matches!(self.asleep, Some(Why::Link | Why::Flat))
            || self.state == MD_STATE_DISCONNECTED
                && !radio::pairing()
                && now.wrapping_sub(self.state_since) >= RECONNECT_TIMEOUT_MS
    }

    /// Asleep: whether to stop now. Only on the battery: with the cable in,
    /// USB may be up, and stopping its clock would drop it.
    fn stop_due(&mut self, now: u32) -> bool {
        let mode = match stop_mode() {
            StopMode::Off => false,
            StopMode::Always => true,
            StopMode::Link => self.host_gone(now),
        };
        mode && now.wrapping_sub(self.asleep_at) >= STOP_AFTER_MS
            && now.wrapping_sub(self.stopped_at) >= STOP_DELAY_MS
            && self.cable.is_low()
            && uart::idle()
            && (lower() || stop::rows_high())
    }

    fn stop(&mut self) {
        let Some(fired) = stop::stop(lower(), stop_diag()) else {
            STOP_ABORTS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.stopped_at = now_ms();
        STOPS.fetch_add(1, Ordering::Relaxed);
        if fired & (stop::ROW_LINES | stop::KNOB_LINE | stop::CABLE_LINE) != 0 {
            STOP_WAKES.fetch_add(1, Ordering::Relaxed);
        }
        if fired & (stop::ROW_LINES | stop::KNOB_LINE) != 0 {
            input();
        }
    }

    fn wake(&mut self) {
        let now = now_ms();
        LOWER.store(false, Ordering::Relaxed);
        FLAT.store(false, Ordering::Relaxed);
        HOST_SUSPENDED.store(false, Ordering::Relaxed);
        LAST_INPUT.store(now, Ordering::Relaxed);
        self.state_since = now;
        self.asleep = None;
        crate::scan::poke();
        refresh_caps();
        radio::reselect();
    }
}

impl rmk::core_traits::Runnable for Power {
    async fn run(&mut self) -> ! {
        loop {
            let wait = if self.asleep.is_some() {
                TICK_MS
            } else {
                self.idle_left(now_ms()).min(AWAKE_TICK_MS)
            };
            select(POKE.wait(), Timer::after_millis(wait as u64)).await;
            let now = now_ms();
            if self.asleep.is_none() {
                if let Some(why) = self.due(now) {
                    self.sleep(why).await;
                }
            } else if self.woken() {
                self.wake();
            } else if self.flat_while_asleep() {
                self.sleep(Why::Flat).await;
            } else if self.stop_due(now) {
                self.stop();
                if self.woken() {
                    self.wake();
                }
            }
        }
    }
}

/// The Caps Lock LED's pin for RMK's indicator: off while asleep, and on
/// USB while it isn't configured or is suspended; back to what RMK last set
/// after (`refresh_caps`).
pub struct CapsPin(Output);

static CAPS_ON: AtomicBool = AtomicBool::new(false);

impl CapsPin {
    pub fn new(pin: Output) -> Self {
        CapsPin(pin)
    }
}

/// Whether the Caps Lock LED may be on (5075b_plus.c's housekeeping).
fn caps_allowed() -> bool {
    !sleeping() && (radio::wireless() || USB_ACTIVE.load(Ordering::Relaxed))
}

/// Applies the Caps Lock LED's state (active low): see `CapsPin`.
pub fn refresh_caps() {
    // SAFETY: C5 is only ever written as the Caps Lock LED's output.
    let pin = unsafe { AnyPin::steal(Port::C, 5) };
    let on = CAPS_ON.load(Ordering::Relaxed) && caps_allowed();
    pin.set_level((!on).into());
}

impl embedded_hal::digital::ErrorType for CapsPin {
    type Error = Infallible;
}

impl embedded_hal::digital::OutputPin for CapsPin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        CAPS_ON.store(true, Ordering::Relaxed);
        if caps_allowed() {
            self.0.set_low();
        }
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        CAPS_ON.store(false, Ordering::Relaxed);
        self.0.set_high();
        Ok(())
    }
}

impl embedded_hal::digital::StatefulOutputPin for CapsPin {
    fn is_set_high(&mut self) -> Result<bool, Infallible> {
        Ok(!CAPS_ON.load(Ordering::Relaxed))
    }

    fn is_set_low(&mut self) -> Result<bool, Infallible> {
        Ok(CAPS_ON.load(Ordering::Relaxed))
    }
}
