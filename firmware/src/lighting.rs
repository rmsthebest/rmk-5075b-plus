//! RGB lighting: the vendor's board lighting (the `lighting` crate's
//! `Board`: QMK's rgb_matrix, the side lights, 5075b_plus.c's RGB keys and
//! blinks, recorded colours) on the 106 WS2812 LEDs (wb32_hal::ws2812, SPIM2
//! on B15).
//!
//! The task runs a frame every 16 ms (RGB_MATRIX_LED_FLUSH_LIMIT), all
//! chunks of it back to back (QMK spreads them over main-loop passes a
//! fraction of a millisecond apart), and sends it only when an LED changed.
//! Key presses go to the hit tracker for the reactive effects, except the
//! two positions the Win/Mac switch taps (keymap::SWITCH_*): on QMK the
//! switch is a GPIO, not a key, and (5, 3) is under LED 105, a side light.
//!
//! Keys reach `Board::key` as ActionEvents. `action_hook`, which RMK's
//! keyboard calls first (third_party/rmk: set_action_hook), keeps from the
//! keyboard what the board takes: key presses while recording colours, and
//! EE_CLR, whose release RMK only sees once its 3 s hold has reset the
//! settings (RMK then erases its storage, the keymap's, and reboots; QMK
//! resets in place). As QMK's, the reset keeps the selected device: the
//! defaults are saved with it, so the keyboard comes back on it.
//! Unlike QMK, recording lets key releases through, so a key held when it
//! starts doesn't stick.
//!
//! Power: A8 enables the LEDs' supply, as the vendor's code sets it, or
//! for the lights-off battery flash (`Board::led_power`); it goes on before
//! a frame is sent. A9, the boost converter, is on but while asleep.
//!
//! Sleep (src/power.rs): asleep, the lighting is suspended (black, then
//! the supply off; `Board::set_sleep`) and the aux and Win-lock LEDs are
//! off; on waking it comes back as it was. On USB the keyboard doesn't
//! sleep, but the lighting is suspended 7 s after the host suspends
//! (USB_POWER_DOWN_DELAY, the vendor's suspend_power_down), until it
//! resumes. A flat battery's request (`Vendor::shutdown_requested`) goes to
//! `power::flat_battery`.
//!
//! The USB host gets the battery level and charge state too (M7e, RMK's
//! `hid::set_battery`; Linux shows a power supply): a second after USB is
//! configured, once the module has reported a level, and on each change
//! (`report_battery`).
//!
//! Saving: what QMK keeps in its EEPROM (`akko_lighting::record`) goes to
//! its own two sectors of the SPI flash, after RMK's window, written in
//! turn: each save erases the older one, so a power cut mid-write leaves
//! the other. A save waits until `SAVE_AFTER` without a key, and is
//! skipped when nothing changed (the CRC). EE_CLR's reset saves the
//! defaults before RMK sees the release. A save holds up this task (a sector erase
//! and three page programs, about 50 ms on a typical part); since RMK's
//! action channel waits rather than drops, more than its 16 actions in that
//! time would delay the keyboard, never lose keys.
//!
//! Vial's lighting tab speaks VialRGB (`akko_lighting::vialrgb`) through
//! RMK's custom value hook (src/main.rs). Its reads answer from a copy of
//! the effect kept here; its changes go to the task, which applies them at
//! the next frame. As with a key, a change is saved after `SAVE_AFTER`
//! (vial-qmk keeps it unsaved until Vial's save); Vial's save, saves now.
//!
//! Keyboard settings the vendor keeps beside the lighting, saved in the
//! same record (`record::FLAG_*`):
//! - GU_TOGG (QMK's magic keycode, User(0x8B) through third_party/rmk)
//!   turns the GUI keys off in Windows mode, and the Win-lock LED (B0,
//!   active low) on. As on QMK, Mac mode ignores it.
//! - HS_CT_A held 3 s makes Right Ctrl send App, or back, with a black
//!   blink; `action_hook` swaps the action.
//! - HS_DIR (Fn+W) swaps WASD and the arrows, wherever they are in the
//!   keymap, with a black blink; `action_hook` swaps the action.
//!
//! `action_hook` also makes the edit keys (KC_CUT/COPY/PSTE/FIND) Ctrl or
//! Cmd shortcuts, and with the `mac` feature sends the vendor's HS_SIRI and
//! Mission Control (src/main.rs has the rest of it).
//!
//! - Whether key presses wait out the debounce (src/scan.rs), set over
//!   Vial channel 0xD6 (src/main.rs).
//! - The radio's selected device and last BT device (src/radio.rs), as
//!   QMK's confinfo.devs and last_btdevs. The radio changes them on a
//!   pairing key, whose action also starts this record's save timer.
//!
//! The aux LED (B9) is high while the lighting is on (RGB_TOG) and no
//! cable is in (A7, the cable sense, low), as 5075b_plus.c sets it. The
//! Win-lock LED is off while USB is suspended, as the vendor's
//! usb_suspend_flag keeps it.
//!
//! The connection indicator (LEDs 37-41) follows the radio (src/radio.rs):
//! its device changes and the module's state go to the board before each
//! frame.
//!
//! The battery indicators (LED 0, and Fn+Space's bar on 16-25) take the
//! level and charge state from the radio (`radio::battery`) before each
//! frame.
//!
//! The factory LED test (Left Ctrl held at power-up: red, green, blue,
//! white, 4 s each) is the board's; src/main.rs reads the key.
//!
//! Not yet: NK_TOGG (RMK has no NKRO).

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

use akko_lighting::board::{CHANNELS, KEYS};
use akko_lighting::engine::MAX_BRIGHTNESS;
use akko_lighting::record::{self, Record};
use akko_lighting::vialrgb;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;

use crate::keymap::{layer, user};
use crate::{power, radio};
use akko_lighting::{Board, Key};
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};
use rmk::event::{ActionEvent, ConnectionStatusChangeEvent, KeyboardEvent, KeyboardEventSource};
use rmk::hid::BatteryReport;
use rmk::macros::processor;
use rmk::processor::DeadlineProcessor;
use rmk::types::action::{Action, KeyboardAction, LightAction};
use rmk::types::connection::UsbState;
use rmk::types::keycode::{HidKeyCode, KeyCode};
use rmk::types::modifier::ModifierCombination;
use wb32_hal::gpio::{Input, Output};
use wb32_hal::spi_nor::{Partition, SECTOR_SIZE};
use wb32_hal::ws2812::Ws2812;

use crate::keymap::{SWITCH_MAC, SWITCH_WIN};

/// The board is recording colours: key presses are its.
static RECORDING: AtomicBool = AtomicBool::new(false);
/// EE_CLR's hold has reset the lighting: its release may go to RMK.
static EE_CLR_DONE: AtomicBool = AtomicBool::new(false);
/// A Shift was held at the last action (QMK's RGB keys go backwards with
/// it). Read as the action's event is handled, a moment later.
static SHIFTED: AtomicBool = AtomicBool::new(false);
/// The Win/Mac switch is on Mac (its last DefaultLayer tap).
static MAC: AtomicBool = AtomicBool::new(false);
/// GU_TOGG's state (QMK's keymap_config.no_gui).
static NO_GUI: AtomicBool = AtomicBool::new(false);
/// HS_CT_A's state (the vendor's confinfo.ctrl_app_flag).
static CTRL_APP: AtomicBool = AtomicBool::new(false);
/// HS_DIR's state (the vendor's confinfo.dir_flag).
static DIR: AtomicBool = AtomicBool::new(false);

/// The lighting's sectors on the SPI flash: two, after RMK's window
/// (src/main.rs).
pub const STORE_START: u32 = 0x2_0000;
pub const STORE_LEN: u32 = 2 * SECTOR_SIZE;
/// A save waits this long after the last key.
const SAVE_AFTER: u32 = 3000;

/// The saved lighting on the flash (see the top of this file).
pub struct Store {
    flash: Partition<'static>,
    /// The record read at start-up, then each one written.
    buf: &'static mut [u8; record::LEN],
    /// The sector with the newest good record, its seq and CRC.
    current: Option<u32>,
    seq: u32,
    crc: u32,
    save_at: Option<u32>,
    /// A save asked for (Vial's), which keys don't put off.
    asked: bool,
}

impl Store {
    /// Finds the newest good record, leaving it in `buf` (see `record`).
    pub async fn load(mut flash: Partition<'static>, buf: &'static mut [u8; record::LEN]) -> Self {
        let mut seqs = [None; 2];
        for (i, seq) in seqs.iter_mut().enumerate() {
            if flash.read(i as u32 * SECTOR_SIZE, buf).await.is_ok() {
                *seq = Record::check(buf).map(Record::seq);
            }
        }
        let current = match seqs {
            [Some(a), Some(b)] => Some(if b.wrapping_sub(a) as i32 > 0 { 1 } else { 0 }),
            [Some(_), None] => Some(0),
            [None, Some(_)] => Some(1),
            [None, None] => None,
        };
        let mut s = Store {
            flash,
            buf,
            current,
            seq: 0,
            crc: 0,
            save_at: None,
            asked: false,
        };
        if let Some(c) = current {
            // The second read left sector 1 in `buf`.
            if c == 0 && s.flash.read(0, s.buf).await.is_err() {
                s.current = None;
            }
        }
        match s.record() {
            Some(r) => {
                s.seq = r.seq();
                s.crc = record::stored_crc(s.buf);
            }
            None => s.current = None,
        }
        s
    }

    /// The record found by `load`, if any (until the first save).
    pub fn record(&self) -> Option<&Record> {
        self.current.and(Record::check(&*self.buf))
    }

    fn touch(&mut self, now: u32) {
        if self.asked {
            return;
        }
        self.save_at = Some(now.wrapping_add(SAVE_AFTER));
    }

    fn due(&self, now: u32) -> bool {
        self.save_at
            .is_some_and(|t| now.wrapping_sub(t) as i32 >= 0)
    }

    /// Writes `board`'s settings if they changed since the last save.
    async fn save(&mut self, board: &Board) {
        self.save_at = None;
        self.asked = false;
        record::encode(
            board,
            flags(),
            flags2(),
            crate::power::setting(),
            self.seq,
            self.buf,
        );
        if self.current.is_some() && record::stored_crc(self.buf) == self.crc {
            return;
        }
        let seq = self.seq.wrapping_add(1);
        record::encode(
            board,
            flags(),
            flags2(),
            crate::power::setting(),
            seq,
            self.buf,
        );
        let to = match self.current {
            Some(0) => 1,
            _ => 0,
        };
        let at = to * SECTOR_SIZE;
        if self.flash.erase(at, at + SECTOR_SIZE).await.is_err()
            || self.flash.write(at, &self.buf[..]).await.is_err()
        {
            return;
        }
        self.current = Some(to);
        self.seq = seq;
        self.crc = record::stored_crc(self.buf);
    }
}

/// VialRGB's mode: id (little-endian), speed, h, s, v.
type VialMode = [u8; 6];
/// The effect as Vial sees it, kept by the task.
static VIAL_MODE: Mutex<CriticalSectionRawMutex, Cell<VialMode>> = Mutex::new(Cell::new([0; 6]));
/// A mode set from Vial, for the task.
static VIAL_SET: Mutex<CriticalSectionRawMutex, Cell<Option<VialMode>>> =
    Mutex::new(Cell::new(None));
static VIAL_SAVE: AtomicBool = AtomicBool::new(false);

const VIA_SET_VALUE: u8 = 0x07;
const VIA_GET_VALUE: u8 = 0x08;
const VIA_SAVE: u8 = 0x09;

/// VialRGB, from RMK's custom value hook: `buf` is the 32-byte reply,
/// pre-filled with the request. False if it isn't a VialRGB command.
pub fn vialrgb(buf: &mut [u8]) -> bool {
    if buf.len() < 32 {
        return false;
    }
    match (buf[0], buf[1]) {
        (VIA_GET_VALUE, vialrgb::GET_INFO) => {
            buf[2..4].copy_from_slice(&vialrgb::PROTOCOL_VERSION.to_le_bytes());
            buf[4] = MAX_BRIGHTNESS;
        }
        (VIA_GET_VALUE, vialrgb::MODE) => buf[2..8].copy_from_slice(&VIAL_MODE.lock(Cell::get)),
        (VIA_GET_VALUE, vialrgb::GET_SUPPORTED) => {
            let gt = u16::from_le_bytes([buf[2], buf[3]]);
            vialrgb::supported_after(gt, &mut buf[2..32]);
        }
        (VIA_SET_VALUE, vialrgb::MODE) => {
            let mut m: VialMode = buf[2..8].try_into().unwrap();
            m[5] = m[5].min(MAX_BRIGHTNESS);
            VIAL_SET.lock(|c| c.set(Some(m)));
            VIAL_MODE.lock(|c| c.set(m));
        }
        (VIA_SAVE, _) => VIAL_SAVE.store(true, Ordering::Relaxed),
        _ => return false,
    }
    true
}

/// HS_CT_A's hold.
const CT_A_HOLD: u32 = 3000;
/// How long after USB is configured the battery report waits: the host's
/// HID driver binds after the configuration, and a report it doesn't read
/// is dropped (RMK's 20 ms cap).
const BATTERY_REPORT_DELAY_MS: u32 = 1_000;

/// USB_POWER_DOWN_DELAY: the host suspended this long turns the lights off.
const USB_LIGHTS_OFF_MS: u32 = 7000;
/// Frames while the lights are suspended (asleep, or the USB host
/// suspended): rgb_matrix's suspend sends nothing then, so a frame only
/// looks for the lights coming back on (a host's resume, the cable), which
/// can wait this long. A key wakes it at once.
const SUSPENDED_FRAME_MS: u32 = 250;

/// The board's key for an action.
fn board_key(a: &Action) -> Key {
    match *a {
        Action::Light(l) => match l {
            LightAction::RgbTog => Key::RgbTog,
            LightAction::RgbModeForward => Key::RgbMod,
            LightAction::RgbModeReverse => Key::RgbRmod,
            LightAction::RgbHui => Key::RgbHui,
            LightAction::RgbHud => Key::RgbHud,
            LightAction::RgbSai => Key::RgbSai,
            LightAction::RgbSad => Key::RgbSad,
            LightAction::RgbVai => Key::RgbVai,
            LightAction::RgbVad => Key::RgbVad,
            LightAction::RgbSpi => Key::RgbSpi,
            LightAction::RgbSpd => Key::RgbSpd,
            _ => Key::Other,
        },
        Action::User(n @ user::RP_P0..user::RP_END) => Key::RpPlay(n - user::RP_P0),
        Action::User(user::RP_END) => Key::RpEnd,
        Action::User(user::RL_MOD) => Key::RlMod,
        Action::User(user::KC_ENCW) => Key::Encw,
        Action::User(user::HS_BATQ) => Key::BatQ,
        Action::KeyboardControl(KeyboardAction::ClearEeprom) => Key::EeClr,
        // The Fn layers, whose MO keys recording lets through.
        Action::LayerOn(layer::FL | layer::MFL) => Key::Fn,
        Action::LayerOn(_) => Key::Momentary,
        _ => Key::Other,
    }
}

/// The saved keyboard flags (`record::FLAG_*`), the radio's device among them.
fn flags() -> u8 {
    let mut f = crate::radio::saved_flags();
    if NO_GUI.load(Ordering::Relaxed) {
        f |= record::FLAG_NO_GUI;
    }
    if CTRL_APP.load(Ordering::Relaxed) {
        f |= record::FLAG_CTRL_APP;
    }
    if crate::scan::defer_press() {
        f |= record::FLAG_DEFER_PRESS;
    }
    f
}

/// The second flags byte (`record::FLAG2_*`).
fn flags2() -> u8 {
    if DIR.load(Ordering::Relaxed) {
        record::FLAG2_DIR
    } else {
        0
    }
}

/// The saved keyboard flags at start-up, before `action_hook` is set.
pub fn set_flags(f: u8, f2: u8) {
    NO_GUI.store(f & record::FLAG_NO_GUI != 0, Ordering::Relaxed);
    CTRL_APP.store(f & record::FLAG_CTRL_APP != 0, Ordering::Relaxed);
    crate::scan::set_defer_press(f & record::FLAG_DEFER_PRESS != 0);
    DIR.store(f2 & record::FLAG2_DIR != 0, Ordering::Relaxed);
}

/// HS_DIR's swap: WASD for the arrows and back (5075b_plus.c's KC_A..KC_UP
/// cases).
fn dir_swap(k: HidKeyCode) -> Option<HidKeyCode> {
    use HidKeyCode::*;
    Some(match k {
        W => Up,
        A => Left,
        S => Down,
        D => Right,
        Up => W,
        Left => A,
        Down => S,
        Right => D,
        _ => return None,
    })
}

/// The board is recording colours.
pub fn recording() -> bool {
    RECORDING.load(Ordering::Relaxed)
}

/// Saves the record (with the keyboard flags) at the next frame, as Vial's
/// save does.
pub fn request_save() {
    VIAL_SAVE.store(true, Ordering::Relaxed);
}

/// RMK's action hook (see the top of this file): it may swap the action;
/// false keeps it from the keyboard.
pub fn action_hook(a: &mut Action, e: &KeyboardEvent, mods: ModifierCombination) -> bool {
    SHIFTED.store(mods.left_shift() || mods.right_shift(), Ordering::Relaxed);
    match *a {
        // The Win/Mac switch's taps: a GPIO on QMK, never the board's.
        Action::DefaultLayer(layer) => {
            MAC.store(layer == layer::MBL, Ordering::Relaxed);
            return true;
        }
        Action::User(user::GU_TOGG) => {
            if e.pressed {
                NO_GUI.fetch_xor(true, Ordering::Relaxed);
            }
            return true;
        }
        Action::User(user::HS_DIR) => {
            if e.pressed {
                DIR.fetch_xor(true, Ordering::Relaxed);
            }
            return true;
        }
        // keycode_config(): no GUI in Windows mode. Releases pass, so a GUI
        // held across GU_TOGG doesn't stick.
        Action::Key(KeyCode::Hid(HidKeyCode::LGui | HidKeyCode::RGui))
            if e.pressed && NO_GUI.load(Ordering::Relaxed) && !MAC.load(Ordering::Relaxed) =>
        {
            return false;
        }
        Action::Key(KeyCode::Hid(HidKeyCode::RCtrl)) if CTRL_APP.load(Ordering::Relaxed) => {
            *a = Action::Key(KeyCode::Hid(HidKeyCode::Application));
        }
        Action::Key(KeyCode::Hid(k)) if DIR.load(Ordering::Relaxed) && dir_swap(k).is_some() => {
            *a = Action::Key(KeyCode::Hid(dir_swap(k).unwrap()));
        }
        // The edit keys as the shortcuts every OS knows (the HID usages
        // mostly do nothing): Ctrl in Windows mode, Cmd in Mac mode. The
        // vendor sends Ctrl in both.
        Action::Key(KeyCode::Hid(
            k @ (HidKeyCode::Cut | HidKeyCode::Copy | HidKeyCode::Paste | HidKeyCode::Find),
        )) => {
            let key = match k {
                HidKeyCode::Cut => HidKeyCode::X,
                HidKeyCode::Copy => HidKeyCode::C,
                HidKeyCode::Paste => HidKeyCode::V,
                _ => HidKeyCode::F,
            };
            let m = if MAC.load(Ordering::Relaxed) {
                ModifierCombination::LGUI
            } else {
                ModifierCombination::LCTRL
            };
            *a = Action::KeyWithModifier(key, m);
        }
        // The vendor's Mac keys (feature `mac`): HS_SIRI is Cmd+Space, and
        // Mission Control is Ctrl+Up rather than its consumer usage.
        #[cfg(feature = "mac")]
        Action::User(user::HS_SIRI) => {
            *a = Action::KeyWithModifier(HidKeyCode::Space, ModifierCombination::LGUI)
        }
        #[cfg(feature = "mac")]
        Action::Key(KeyCode::Hid(HidKeyCode::MissionControl)) => {
            *a = Action::KeyWithModifier(HidKeyCode::Up, ModifierCombination::LCTRL);
        }
        _ => {}
    }
    let key = board_key(a);
    if key == Key::EeClr {
        return !e.pressed && EE_CLR_DONE.load(Ordering::Relaxed);
    }
    let exempt = matches!(key, Key::RpPlay(_) | Key::RpEnd | Key::RgbMod | Key::Fn);
    !(RECORDING.load(Ordering::Relaxed) && e.pressed && !exempt)
}

#[processor(subscribe = [KeyboardEvent, ActionEvent, ConnectionStatusChangeEvent])]
#[::rmk::macros::runnable_generated]
pub struct Lighting {
    board: &'static mut Board,
    store: Store,
    leds: Ws2812,
    power: Output,
    win_lock_led: Output,
    aux_led: Output,
    cable: Input,
    /// A9, the LEDs' boost converter.
    boost: Output,
    /// When HS_CT_A went down, while it's held and hasn't fired.
    ct_a_since: Option<u32>,
    /// When USB was suspended, while it is.
    usb_suspended_at: Option<u32>,
    /// When the last frame ran (ms), for `SUSPENDED_FRAME_MS`.
    frame_at: u32,
    /// When USB was configured, while it is: the battery report waits
    /// BATTERY_REPORT_DELAY_MS for the host's driver.
    usb_configured_at: Option<u32>,
    /// The battery report the host has (`report_battery`).
    battery_sent: Option<BatteryReport>,
}

impl Lighting {
    /// `board` from `Board::new_in` with `store.record()`, kept outside the
    /// main task (src/main.rs `ram2`).
    pub fn new(
        board: &'static mut Board,
        store: Store,
        leds: Ws2812,
        power: Output,
        win_lock_led: Output,
        aux_led: Output,
        cable: Input,
        boost: Output,
    ) -> Self {
        let mut l = Lighting {
            board,
            store,
            leds,
            power,
            win_lock_led,
            aux_led,
            cable,
            boost,
            ct_a_since: None,
            usb_suspended_at: None,
            frame_at: 0,
            usb_configured_at: None,
            battery_sent: None,
        };
        l.update_power();
        l
    }

    /// The default recordings, for `Board::new_in`.
    pub fn recordings() -> &'static [[u16; KEYS]; CHANNELS] {
        &crate::keymap::RECORDINGS
    }

    fn update_power(&mut self) {
        if self.board.led_power() {
            self.power.set_high();
        } else {
            self.power.set_low();
        }
    }

    /// The Win-lock and aux LEDs, as 5075b_plus.c's housekeeping sets them.
    fn update_leds(&mut self) {
        let asleep = power::sleeping();
        let usb_suspended = !radio::wireless() && self.usb_suspended_at.is_some();
        let locked = NO_GUI.load(Ordering::Relaxed)
            && !MAC.load(Ordering::Relaxed)
            && !asleep
            && !usb_suspended;
        self.win_lock_led.set_level((!locked).into());
        let aux = self.board.vendor.conf.rgb_enable && self.cable.is_low() && !asleep;
        self.aux_led.set_level(aux.into());
        self.boost.set_level((!asleep).into());
    }

    /// Asleep, or on USB with the host suspended for USB_LIGHTS_OFF_MS.
    fn lights_off(&self, now: u32) -> bool {
        power::sleeping()
            || (!radio::wireless()
                && self
                    .usb_suspended_at
                    .is_some_and(|t| now.wrapping_sub(t) >= USB_LIGHTS_OFF_MS))
    }

    /// HS_CT_A's hold, as the vendor's housekeeping times it.
    fn ct_a(&mut self, now: u32) {
        if let Some(t) = self.ct_a_since {
            if now.wrapping_sub(t) > CT_A_HOLD {
                self.ct_a_since = None;
                CTRL_APP.fetch_xor(true, Ordering::Relaxed);
                self.board.confirm_blink(now);
                self.store.touch(now);
            }
        }
    }

    /// A mode set from Vial, as vialrgb.c's set_mode with the _noeeprom
    /// setters.
    fn apply_vial(&mut self, now: u32) {
        if let Some(m) = VIAL_SET.lock(|c| c.take()) {
            let id = u16::from_le_bytes([m[0], m[1]]);
            vialrgb::set_mode(&mut self.board.engine, id, m[2], m[3], m[4], m[5]);
            self.store.touch(now);
        }
        if VIAL_SAVE.swap(false, Ordering::Relaxed) {
            self.store.save_at = Some(now);
            self.store.asked = true;
        }
    }

    /// After anything that may have changed the board's state.
    async fn sync(&mut self) {
        let c = &self.board.engine.config;
        let id = if c.enable {
            vialrgb::to_vial(c.mode)
        } else {
            vialrgb::OFF
        };
        let [lo, hi] = id.to_le_bytes();
        VIAL_MODE.lock(|m| m.set([lo, hi, c.speed, c.hsv.h, c.hsv.s, c.hsv.v]));
        self.update_power();
        self.update_leds();
        RECORDING.store(self.board.recording(), Ordering::Relaxed);
        if self.board.vendor.reset_requested {
            self.board.vendor.reset_requested = false;
            // hs_reset_settings: the keyboard's settings too, but the
            // selected device (eeconfig_confinfo_default).
            NO_GUI.store(false, Ordering::Relaxed);
            CTRL_APP.store(false, Ordering::Relaxed);
            DIR.store(false, Ordering::Relaxed);
            crate::scan::set_defer_press(false);
            power::set_setting(record::SLEEP_DEFAULT);
            radio::reset_saved();
            self.store.save(self.board).await;
            EE_CLR_DONE.store(true, Ordering::Relaxed);
        }
    }

    async fn on_keyboard_event(&mut self, e: KeyboardEvent) {
        if let KeyboardEventSource::Key(k) = e.source {
            if (k.row, k.col) != SWITCH_WIN && (k.row, k.col) != SWITCH_MAC {
                self.board.engine.key_event(k.row, k.col, e.pressed);
            }
        }
    }

    async fn on_action_event(&mut self, e: ActionEvent) {
        if matches!(e.action, Action::DefaultLayer(_)) {
            self.update_leds();
            return;
        }
        if e.action == Action::User(user::HS_CT_A) {
            self.ct_a_since = e.keyboard_event.pressed.then(now_ms);
        }
        if e.action == Action::User(user::HS_DIR) && e.keyboard_event.pressed {
            self.board.confirm_blink(now_ms());
        }
        // Encoder turns have no key; the board records nothing for them.
        let (row, col) = match e.keyboard_event.source {
            KeyboardEventSource::Key(k) => (k.row, k.col),
            _ => (u8::MAX, u8::MAX),
        };
        let key = board_key(&e.action);
        let pressed = e.keyboard_event.pressed;
        if key == Key::EeClr && pressed {
            EE_CLR_DONE.store(false, Ordering::Relaxed);
        }
        let now = now_ms();
        self.board
            .key(key, row, col, pressed, SHIFTED.load(Ordering::Relaxed), now);
        self.store.touch(now);
        self.sync().await;
    }

    async fn on_connection_status_change_event(&mut self, e: ConnectionStatusChangeEvent) {
        self.board.vendor.usb_active = e.0.usb == UsbState::Configured;
        power::set_usb_active(e.0.usb == UsbState::Configured);
        self.usb_suspended_at = match e.0.usb {
            UsbState::Suspended => self.usb_suspended_at.or(Some(now_ms())),
            _ => None,
        };
        // Suspended keeps the host's copy; anything else but configured
        // loses it.
        match e.0.usb {
            UsbState::Configured => {
                self.usb_configured_at = self.usb_configured_at.or(Some(now_ms()))
            }
            UsbState::Suspended => {}
            _ => (self.usb_configured_at, self.battery_sent) = (None, None),
        }
        self.update_leds();
    }

    /// The battery report to the USB host (M7e): once the module has told
    /// the level, BATTERY_REPORT_DELAY_MS after USB is configured, and on
    /// each change. RMK drops it while suspended; it's sent again then.
    /// The host's GET_REPORT is answered from `set_battery_now` at once.
    fn report_battery(&mut self, now: u32) {
        if !radio::battery_known() {
            return;
        }
        let v = &self.board.vendor;
        let report = BatteryReport {
            level: v.bat,
            charging: v.charging && !v.full,
        };
        rmk::hid::set_battery_now(report);
        let Some(at) = self.usb_configured_at else {
            return;
        };
        if now.wrapping_sub(at) < BATTERY_REPORT_DELAY_MS {
            return;
        }
        if self.usb_suspended_at.is_some() {
            self.battery_sent = None;
        } else if self.battery_sent != Some(report) {
            rmk::hid::set_battery(report);
            self.battery_sent = Some(report);
        }
    }

    /// Runs one whole frame: rgb_matrix_task() until the frame is sent (or
    /// found unchanged).
    async fn frame(&mut self) {
        let now = now_ms();
        self.frame_at = now;
        self.apply_vial(now);
        self.ct_a(now);
        // The radio's device changes and module state (src/radio.rs).
        while let Some(e) = radio::take_board_event() {
            match e {
                radio::BoardEvent::Select(dev) => self.board.devs_change(dev, false, now),
                radio::BoardEvent::Key(dev) => self.board.wls_key(dev, now),
                radio::BoardEvent::Pair(dev) => self.board.wls_pair(dev, now),
            }
        }
        self.board.vendor.md_state = radio::module_state();
        let v = &mut self.board.vendor;
        (v.bat, v.charging, v.full) = radio::battery();
        let off = self.lights_off(now);
        if off != self.board.engine.suspended {
            if self.board.set_sleep(off, now) {
                self.leds
                    .write(self.board.engine.leds.rgb.iter().map(|c| [c.r, c.g, c.b]))
                    .await;
            }
            self.update_power();
            self.update_leds();
        }
        loop {
            if self.board.task_sparse(now) {
                self.update_power();
                self.leds
                    .write(self.board.engine.leds.rgb.iter().map(|c| [c.r, c.g, c.b]))
                    .await;
            }
            if !self.board.engine.frame_due(now) {
                break;
            }
            // Between chunks: the scan and the rest get their turn, so a
            // heavy effect's frame doesn't delay a key by its whole length.
            embassy_futures::yield_now().await;
        }
        if core::mem::take(&mut self.board.vendor.shutdown_requested) {
            power::flat_battery();
        }
        self.report_battery(now);
        // EE_CLR's reset happens in a frame.
        self.sync().await;
    }
}

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

impl Lighting {
    /// Suspended: ms until the next frame (0: due).
    fn suspended_wait(&self, now: u32) -> u32 {
        SUSPENDED_FRAME_MS.saturating_sub(now.wrapping_sub(self.frame_at))
    }
}

impl DeadlineProcessor for Lighting {
    fn deadline(&self) -> Option<Instant> {
        let now = now_ms();
        let engine = &self.board.engine;
        // Coming back from suspended: at once.
        let resumed = engine.suspended && !self.lights_off(now);
        let frame = if engine.suspended {
            resumed || self.suspended_wait(now) == 0
        } else {
            engine.frame_due(now)
        };
        if frame || self.store.due(now) {
            return Some(Instant::now());
        }
        let mut wait = if engine.suspended {
            self.suspended_wait(now)
        } else {
            engine.next_frame().wrapping_sub(now)
        };
        if let Some(t) = self.store.save_at {
            wait = wait.min(t.wrapping_sub(now));
        }
        Some(Instant::now() + embassy_time::Duration::from_millis(wait as u64))
    }

    async fn on_deadline(&mut self) {
        if self.store.due(now_ms()) {
            self.store.save(self.board).await;
        }
        let now = now_ms();
        let engine = &self.board.engine;
        let due = if engine.suspended {
            self.suspended_wait(now) == 0 || !self.lights_off(now)
        } else {
            engine.frame_due(now)
        };
        if due {
            self.frame().await;
        }
    }
}

impl rmk::core_traits::Runnable for Lighting {
    async fn run(&mut self) -> ! {
        self.deadline_loop().await
    }
}
