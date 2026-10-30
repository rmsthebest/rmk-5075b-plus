//! The board's own lighting behaviour, from the vendor's QMK code
//! (keyboards/akko/5075b_plus: 5075b_plus.c and rgb_record/): what its RGB
//! keys do around rgb_matrix's, the side lights (`Rgblight`), the
//! whole-board blinks that confirm a key, and the recorded per-key colours
//! that effect 45 (RGBR_PLAY) plays.
//!
//! `Board::key` is process_record_kb (with process_record_hs) followed by
//! QMK's process_rgb for the RGB keys; `Board::task` is a main-loop pass's
//! rgblight_task + rgb_matrix_task, with the board's indicator hooks, after
//! wls.c's hs_rgb_blink_hook (the module state's effects on the lights).
//! The connection indicator (M6) is here: the device's key blinking while
//! the link is down, and shown for 2 s once it's up. The firmware tells the
//! board the device (`devs_change`, `wls_key`, `wls_pair`) and the module's
//! state (`Vendor::md_state`). The hook's sleep timeouts aren't here (M7b).
//!
//! The battery (M7): the firmware sets `Vendor::bat`, `charging` and
//! `full`. As the vendor's: at 15% or less LED 0 blinks; Fn+Space (HS_BATQ) shows the level on LEDs 16-25; at 0% for 60 s
//! `shutdown_requested` asks the firmware to sleep. Changed from the
//! vendor's, which tools/rgbref/board.py applies to the C it compares with:
//! - the low-battery blink is red (the vendor's `(RGB){0xFF, 0, 0}` is
//!   green in this board's g r b struct);
//! - the indicators also show while recording colours;
//! - Fn+Space works on USB too;
//! - 30-49% shows yellow (the vendor's LEVEL2 colour is LEVEL1's green);
//! - LED 0 stays dark while charging (the vendor's is red until full):
//!   Fn+Space and the battery level over Vial show the charge instead.
//! New: with the lights off (`power` false) a low battery flashes LED 0 red
//! for 150 ms every 5 s, the chain powered just for it (`led_power`), and
//! the 0% timer runs whether the lights are on or not.
//!
//! Where the vendor's EEPROM held something (the recorded colour channels,
//! the colour picked for each effect, confinfo), this holds it in RAM; the
//! firmware saves it.

use core::mem::MaybeUninit;

use crate::color::{Hsv, Rgb, hsv_to_rgb};
use crate::effects::effect;
use crate::engine::{Config, Engine, Hooks, Leds, MAX_BRIGHTNESS, VAL_STEP};
use crate::layout::{LED_COUNT, MATRIX_CO, MATRIX_COLS, MATRIX_ROWS, NO_LED};
use crate::record::Record;
use crate::rgblight::{self, Rgblight};

/// Keys in the matrix: one recorded colour each.
pub const KEYS: usize = MATRIX_ROWS * MATRIX_COLS;
/// RGBREC_CHANNEL_NUM.
pub const CHANNELS: usize = 4;
/// The effects RGB_MOD steps through (rgbmatrix_buff); each remembers its
/// own colour, an index into `HSVS`.
pub const MODES: [u8; 16] = [13, 15, 16, 24, 25, 26, 29, 37, 33, 34, 35, 43, 2, 5, 6, 9];
/// rgb_hsvs: the colours RGB_SAI / RGB_SAD step through.
pub(crate) const HSVS: [(u8, u8); 5] = [(0, 255), (64, 255), (128, 255), (192, 255), (0, 0)];
/// RGB_RECORD_HS_LISTS: the colours a key cycles through while recording,
/// as H << 8 | S. 0x0000 is off and 0x0101 white.
const RECORD_COLOURS: [u16; 8] = [
    0x0000, 0x00FF, 0x55FF, 0xAAFF, 0x2BFF, 0xBFFF, 0x6AFF, 0x0101,
];
/// buff[]: the side-light modes RL_MOD steps through. Index 10 is off.
const SIDE_MODES: [u8; 11] = [14, 8, 2, 1, 1, 1, 1, 1, 1, 1, 0];
/// The hues of the static side-light colours, indexes 3 to 8.
const SIDE_HUES: [u8; 6] = [0, 40, 80, 120, 160, 200];
/// Blinks kept at once. The vendor's table has 99 slots; when it's full a
/// new blink is dropped, as here. Blinks only pile up while the lighting is
/// off (they wait for it), so 16 changes nothing in use.
const BLINKS: usize = 16;
/// The LEDs the side lights show on: the last 22.
const SIDE_FIRST: usize = LED_COUNT - rgblight::LED_COUNT;
/// HS_RGB_BLINK_INDEX_USB: the key that shows USB's state.
const LED_USB: usize = 37;
/// HS_MATRIX_BLINK_INDEX_BAT: the battery's LED.
const LED_BAT: usize = 0;
/// RGB_MATRIX_BAT_INDEX_MAP: Fn+Space's bar, a LED per 10%.
const BAT_BAR: [u8; 10] = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25];
/// BATTERY_CAPACITY_LOW and _STOP.
const BAT_LOW: u8 = 15;
const BAT_STOP: u8 = 0;
/// How long the battery may sit at BAT_STOP before the keyboard sleeps.
const BAT_STOP_MS: u32 = 60_000;
/// RGB_MATRIX_BAT_VAL: the bar's brightness.
const BAT_VAL: u8 = 150;
/// The lights-off low-battery flash: every FLASH_EVERY ms the chain is
/// powered, LED 0 lit FLASH_ON..FLASH_OFF ms in (the WS2812s get a frame to
/// wake up first), and the chain off again at FLASH_END.
const FLASH_EVERY: u32 = 5000;
const FLASH_ON: u32 = 20;
const FLASH_OFF: u32 = 170;
const FLASH_END: u32 = 190;
const RED: Rgb = Rgb {
    r: 0xFF,
    g: 0,
    b: 0,
};

/// QMK's DEVS_*.
pub const DEVS_USB: u8 = 0;
pub const DEVS_BT1: u8 = 1;
pub const DEVS_BT2: u8 = 2;
pub const DEVS_BT3: u8 = 3;
pub const DEVS_2G4: u8 = 6;
/// QMK's MD_STATE_* (the module's link state).
pub const MD_STATE_NONE: u8 = 0;
pub const MD_STATE_PAIRING: u8 = 1;
pub const MD_STATE_CONNECTED: u8 = 2;
pub const MD_STATE_DISCONNECTED: u8 = 3;

/// A positional `{r, g, b}` initialiser of QMK's RGB, which on this board
/// is a WS2812 G, R, B struct: the first value lands in green.
const fn grb(first: u8, second: u8, third: u8) -> Rgb {
    Rgb {
        r: second,
        g: first,
        b: third,
    }
}
const WHITE: Rgb = Rgb {
    r: 0xFF,
    g: 0xFF,
    b: 0xFF,
};

/// wireless_devs_change_kb's table, by DEVS_*: the device's key, its
/// re-pairing blink colour (HS_LBACK_COLOR_*) and its reconnecting one
/// (HS_PAIR_COLOR_*). Written as `{RGB_RED}` etc., so through `grb`: BT1
/// blinks green and 2.4G red, though config.h names them the other way.
const WLS_BLINKS: [(u8, Rgb, Rgb); 7] = [
    (37, grb(0xFF, 0xFF, 0xFF), grb(0xFF, 0xFF, 0xFF)),
    (41, grb(0xFF, 0, 0), grb(0xFF, 0, 0)),
    (40, grb(0, 0, 0xFF), grb(0, 0, 0xFF)),
    (39, grb(0xFF, 0xFF, 0), grb(0xFF, 0xFF, 0)),
    (41, grb(0, 0, 0xFF), grb(0, 0, 0xFF)),
    (42, grb(0, 0, 0xFF), grb(0, 0, 0xFF)),
    (38, grb(0, 0xFF, 0), grb(0, 0xFF, 0)),
];

/// rgb_matrix_wls_indicator's state: one blinking key.
#[derive(Clone, Copy)]
struct WlsBlink {
    /// The blink restarts with this `reset` (wls_rgb_indicator_reset).
    reset: bool,
    /// 0: not blinking.
    timer: u32,
    interval: u32,
    times: u32,
    index: u8,
    rgb: Rgb,
}

/// The keycodes the board treats specially.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    RgbTog,
    RgbMod,
    RgbRmod,
    RgbHui,
    RgbHud,
    RgbSai,
    RgbSad,
    RgbVai,
    RgbVad,
    RgbSpi,
    RgbSpd,
    /// RL_MOD: the side lights' next mode.
    RlMod,
    /// RP_P0..RP_P2: play recorded channel n.
    RpPlay(u8),
    /// RP_END: start or end recording.
    RpEnd,
    NkTogg,
    /// EE_CLR: held 3 s, resets the settings.
    EeClr,
    /// KC_ENCW: flips `led_status`, which hides the battery LED.
    Encw,
    /// HS_BATQ (Fn+Space): shows the battery level while held.
    BatQ,
    /// MO(_FL) / MO(_MFL): the Fn key.
    Fn,
    /// Any other MO(layer).
    Momentary,
    Other,
}

/// What `Board::key` tells the keyboard to do with the key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Process it as usual (QMK's process_record_kb returned true).
    Pass,
    /// The board used it up.
    Handled,
}

/// The vendor's confinfo, the lighting part.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfInfo {
    pub record_channel: u8,
    /// The effect RGB_MOD returns to from playback (0xFF: the default).
    pub record_last_mode: u8,
    /// RGB_TOG's state: both the matrix and the side lights.
    pub rgb_enable: bool,
}

impl Default for ConfInfo {
    fn default() -> Self {
        ConfInfo {
            record_channel: 0,
            record_last_mode: 0xFF,
            rgb_enable: true,
        }
    }
}

/// The settings the vendor kept in EEPROM, besides rgb_matrix's config.
#[derive(Clone, PartialEq, Eq)]
pub struct Saved {
    pub rgblight: rgblight::Config,
    pub conf: ConfInfo,
    /// The colour index (into HSVS) picked for each of `MODES`.
    pub colours: [u8; MODES.len()],
    /// The recorded colours, H << 8 | S per key, row-major.
    pub channels: [[u16; KEYS]; CHANNELS],
}

impl Saved {
    /// An erased EEPROM after eeconfig_init: the default recordings
    /// (keymaps/via/keymap.c), everything else default.
    pub fn defaults(default_channels: &[[u16; KEYS]; CHANNELS]) -> Self {
        Saved {
            rgblight: rgblight::Config::default(),
            conf: ConfInfo::default(),
            colours: [0; MODES.len()],
            channels: *default_channels,
        }
    }
}

#[derive(Clone, Copy)]
struct Blink {
    active: bool,
    timer: u32,
    interval: u16,
    /// Half-periods left; odd shows the colour, even black.
    times: u8,
    /// An LED, or 0xFF for all of them.
    index: u8,
    rgb: Rgb,
}

/// For each LED, the first key (row-major) over it: find_matrix_row_col.
const LED_KEY: [u8; LED_COUNT] = {
    let mut t = [0xFF; LED_COUNT];
    let mut k = KEYS;
    while k > 0 {
        k -= 1;
        let i = MATRIX_CO[k / MATRIX_COLS][k % MATRIX_COLS];
        if i != NO_LED {
            t[i as usize] = k as u8;
        }
    }
    t
};

pub struct Vendor {
    pub rgblight: Rgblight,
    pub conf: ConfInfo,
    pub colours: [u8; MODES.len()],
    pub channels: [[u16; KEYS]; CHANNELS],
    default_channels: Option<&'static [[u16; KEYS]; CHANNELS]>,
    /// rgbrec_buffer: the channel being played or recorded.
    rec: [u16; KEYS],
    recording: bool,
    rec_channel: u8,
    /// The brightness recorded colours show at.
    rec_value: u8,
    no_record_fg: bool,
    /// rgblight_mode_index: where RL_MOD is in SIDE_MODES.
    side_index: u8,
    blinks: [Blink; BLINKS],
    ee_clr_timer: u32,
    /// KC_ENCW's flag: no battery LED.
    pub led_status: bool,
    /// The battery %, as the module reports it (md_getp_bat).
    pub bat: u8,
    /// The cable is in (HS_BAT_CABLE_PIN, A7) and the battery is full
    /// (BAT_FULL_PIN, A15).
    pub charging: bool,
    pub full: bool,
    /// rk_bat_req_flag: Fn+Space held.
    bat_req: bool,
    /// hs_rgb_bat: the low-battery blink (HS_RGB_BAT_COUNT is 1).
    bat_blink: Blink,
    /// enable_bat_indicators and bat_indicator_cnt: off for 2 s after
    /// EE_CLR's reset.
    bat_enabled: bool,
    bat_cnt: u32,
    /// battery_process_time: when the battery reached BAT_STOP.
    bat_stop_timer: u32,
    /// The battery sat at BAT_STOP for a minute (the vendor's lower_sleep):
    /// the firmware should sleep, and clear this.
    pub shutdown_requested: bool,
    /// The lights-off flash: when its cycle started (0: not flashing), and
    /// whether it has the chain powered.
    flash_since: u32,
    flash_power: bool,
    /// The lights-off flash is allowed (the reference harness turns it off:
    /// the vendor's C has none).
    pub bat_flash: bool,
    /// LED_POWER_EN_PIN (A8), as the vendor's code last set it.
    pub power: bool,
    /// USB is configured (USB_DRIVER.state == USB_ACTIVE).
    pub usb_active: bool,
    usb_blink_timer: u32,
    usb_blink_state: bool,
    /// hs_devs_indicator_timer and _flag: the device shown after connecting.
    devs_timer: u32,
    devs_flag: bool,
    /// wireless_get_current_devs(): DEVS_*.
    pub devs: u8,
    /// *md_getp_state(): MD_STATE_*, as the module last reported it.
    pub md_state: u8,
    /// hs_pairing_flag: set by a pairing key's 3 s hold.
    pairing: bool,
    /// hs_linker_rgb_timer and hs_rgb_blink_hook's last_status.
    blink_timer: u32,
    last_status: u8,
    wls: WlsBlink,
    /// LCtrl was held at power-up without Esc: the factory LED test.
    factory_test: bool,
    factory_timer: u32,
    factory_step: u8,
    /// EE_CLR fired: the firmware should also reset what isn't lighting
    /// (the keymap), and clear this.
    pub reset_requested: bool,
    now: u32,
}

pub struct Board {
    pub engine: Engine,
    pub vendor: Vendor,
}

impl Board {
    /// The board at power-up: rgb_matrix from its saved config, then
    /// rgblight_init and keyboard_post_init_kb. `None`: nothing saved yet.
    /// `factory_test`: LCtrl held at power-up without Esc.
    pub fn new(
        saved: Option<&Record>,
        default_channels: &'static [[u16; KEYS]; CHANNELS],
        factory_test: bool,
        now: u32,
    ) -> Self {
        let mut slot = MaybeUninit::uninit();
        Self::new_in(&mut slot, saved, default_channels, factory_test, now);
        // SAFETY: new_in initialised it.
        unsafe { slot.assume_init() }
    }

    /// `Board::new`, built in `slot` rather than on the stack (it's 2 KB).
    pub fn new_in<'a>(
        slot: &'a mut MaybeUninit<Board>,
        saved: Option<&Record>,
        default_channels: &'static [[u16; KEYS]; CHANNELS],
        factory_test: bool,
        now: u32,
    ) -> &'a mut Board {
        // SAFETY: Board is plain data (integers, bools, fieldless enums,
        // arrays of them, and an Option<&T>), for which all-zero bytes are
        // valid; the fields that aren't zero are set below.
        let b = unsafe {
            slot.as_mut_ptr().write_bytes(0, 1);
            slot.assume_init_mut()
        };
        b.engine
            .init_zeroed(saved.map_or_else(Config::default, Record::config));
        let v = &mut b.vendor;
        v.rgblight.init_zeroed();
        v.conf = ConfInfo::default();
        v.default_channels = Some(default_channels);
        v.channels = *default_channels;
        v.usb_active = true;
        v.bat = 100;
        v.bat_enabled = true;
        v.bat_cnt = 1;
        v.bat_flash = true;
        v.factory_test = factory_test;
        v.now = now;
        v.rgblight.init(saved.map(Record::rgblight), now);
        match saved {
            Some(s) => {
                v.conf = s.conf();
                v.colours = s.colours();
                for (c, channel) in v.channels.iter_mut().enumerate() {
                    for (k, hs) in channel.iter_mut().enumerate() {
                        *hs = s.channel(c, k);
                    }
                }
            }
            None => v.confinfo_default(),
        }
        let cfg = &mut b.engine.config;
        v.post_init(cfg);
        if !v.rgblight.is_enabled() && !v.conf.rgb_enable {
            b.engine.disable();
            b.vendor.power = false;
        }
        b
    }

    /// What to save, besides `engine.config`.
    pub fn saved(&self) -> Saved {
        let v = &self.vendor;
        Saved {
            rgblight: v.rgblight.config,
            conf: v.conf,
            colours: v.colours,
            channels: v.channels,
        }
    }

    pub fn recording(&self) -> bool {
        self.vendor.recording
    }

    /// wireless_devs_change(current, dev, reset), the board's part: a new
    /// device (or a reset) starts disconnected, and its key blinks.
    pub fn devs_change(&mut self, dev: u8, reset: bool, now: u32) {
        self.vendor.now = now;
        self.vendor.devs_change(dev, reset);
    }

    /// A pairing key's press (process_record_wls): to `dev`, if it isn't
    /// already, and the hold's pairing undone.
    pub fn wls_key(&mut self, dev: u8, now: u32) {
        self.vendor.now = now;
        let v = &mut self.vendor;
        if v.devs != dev {
            v.devs_change(dev, false);
        }
        v.pairing = false;
        v.blink_timer = now;
    }

    /// A pairing key held 3 s (wls_process_long_press): `dev` paired anew.
    pub fn wls_pair(&mut self, dev: u8, now: u32) {
        self.vendor.now = now;
        self.vendor.devs_change(dev, true);
        self.vendor.pairing = true;
    }

    /// The black 250 ms blink the vendor's keys confirm with
    /// (rgb_matrix_hs_indicator_set(0xFF, black, 250, 1)), at `now` ms: for
    /// HS_CT_A's 3 s hold, which the firmware times.
    pub fn confirm_blink(&mut self, now: u32) {
        self.vendor.now = now;
        self.vendor.blink_dir();
    }

    /// One key event through process_record_kb, at `now` ms; (row, col) is
    /// the key's position, `shifted` whether a Shift is held (QMK's RGB keys
    /// go backwards with it).
    pub fn key(
        &mut self,
        key: Key,
        row: u8,
        col: u8,
        pressed: bool,
        shifted: bool,
        now: u32,
    ) -> Outcome {
        self.vendor.now = now;
        let (e, v) = (&mut self.engine, &mut self.vendor);

        // process_record_hs: a key while connected restarts the hook's timer.
        if v.md_state == MD_STATE_CONNECTED && pressed {
            v.blink_timer = now;
        }
        // Recording takes every key but these.
        let exempt = matches!(key, Key::RpPlay(_) | Key::RpEnd | Key::RgbMod | Key::Fn);
        if key == Key::Fn && !pressed && v.recording {
            if v.no_record_fg {
                v.no_record_fg = false;
                v.record(row, col);
            }
            v.no_record_fg = true;
        }
        if v.recording && !exempt {
            if pressed && key != Key::Momentary {
                v.record(row, col);
            }
            return Outcome::Handled;
        }

        match key {
            Key::Encw => {
                if pressed {
                    v.led_status = !v.led_status;
                }
                return Outcome::Handled;
            }
            Key::BatQ => {
                // With the lighting off, the matrix runs just for the bar.
                v.bat_req = pressed;
                if !v.conf.rgb_enable {
                    v.power = pressed;
                    if pressed {
                        e.enable();
                    } else {
                        e.disable();
                    }
                }
                return Outcome::Handled;
            }
            Key::NkTogg => {
                if pressed {
                    // (RGB){0x00, 0x6E, 0x00}: QMK's RGB is in WS2812 order,
                    // g r b, so red; nkr_hook then blinks blue, then green.
                    v.blink(
                        0xFF,
                        Rgb {
                            r: 0x6E,
                            g: 0,
                            b: 0,
                        },
                        250,
                        1,
                    );
                }
                return Outcome::Pass;
            }
            Key::RlMod => {
                if pressed {
                    v.side_next(now);
                }
                return Outcome::Handled;
            }
            Key::EeClr => {
                v.ee_clr_timer = if pressed { now } else { 0 };
                return Outcome::Handled;
            }
            Key::RgbSpi if pressed => {
                if e.config.speed != 255 {
                    v.blink_dir();
                }
            }
            Key::RgbSpd if pressed => {
                if e.config.speed <= 102 {
                    if e.config.speed != 51 {
                        v.blink_dir();
                    }
                    e.set_speed(51);
                    return Outcome::Handled;
                }
                v.blink_dir();
            }
            Key::RgbVai if pressed => {
                if e.config.enable {
                    v.power = true;
                }
                if e.config.hsv.v != MAX_BRIGHTNESS {
                    v.blink_dir();
                }
            }
            Key::RgbVad if pressed => {
                if e.config.hsv.v <= VAL_STEP {
                    v.power = false;
                    e.leds.set_all(Rgb::OFF);
                }
                if e.config.hsv.v != 0 {
                    v.blink_dir();
                }
            }
            Key::RgbSai | Key::RgbSad => {
                if pressed && v.colour_step(e, key == Key::RgbSai) {
                    v.blink_dir();
                }
                if pressed {
                    return Outcome::Handled;
                }
            }
            Key::RpPlay(ch) => {
                if pressed && (ch as usize) < CHANNELS {
                    v.conf.record_channel = ch;
                    v.read_channel(ch);
                    v.rec_end(ch);
                    v.rec_show(e, ch);
                }
                return Outcome::Handled;
            }
            Key::RpEnd => {
                if pressed && e.config.mode == effect::RGBR_PLAY {
                    if !v.recording {
                        v.rec_start(e, v.conf.record_channel);
                        v.no_record_fg = false;
                        v.rec = [0; KEYS];
                    } else {
                        v.rec_end(v.conf.record_channel);
                    }
                }
                return Outcome::Handled;
            }
            Key::RgbMod => {
                if pressed {
                    v.blink_dir();
                    if e.config.mode == effect::RGBR_PLAY {
                        if v.recording {
                            // Leaving playback while recording drops the
                            // recording: the saved channel is read back first.
                            v.read_channel(v.conf.record_channel);
                            v.rec_end(v.conf.record_channel);
                            v.no_record_fg = false;
                        }
                        let m = v.conf.record_last_mode;
                        e.set_mode(if m != 0xFF { m } else { Config::default().mode });
                    } else {
                        v.mode_next(e);
                    }
                }
                return Outcome::Handled;
            }
            Key::RgbHui => {
                if pressed {
                    v.colour_step(e, true);
                }
                return Outcome::Handled;
            }
            Key::RgbTog => {
                if pressed {
                    v.conf.rgb_enable = !v.conf.rgb_enable;
                    if v.conf.rgb_enable {
                        v.rgblight.enable(now);
                        e.enable();
                        if v.side_index == 10 {
                            let hsv = v.rgblight.config.hsv;
                            v.rgblight.sethsv(0, 255, hsv.v);
                            v.rgblight.disable();
                        }
                        v.power = true;
                    } else {
                        v.rgblight.disable();
                        if !v.rgblight.is_enabled() && !v.conf.rgb_enable {
                            e.disable();
                            v.power = false;
                        }
                    }
                }
                return Outcome::Handled;
            }
            _ => {}
        }

        // process_rgb (RGB_TRIGGER_ON_KEYDOWN) for the keys left.
        if !pressed {
            return Outcome::Pass;
        }
        let (up, down): (fn(&mut Engine), fn(&mut Engine)) = match key {
            Key::RgbRmod => (Engine::step_reverse, Engine::step),
            Key::RgbHud => (Engine::decrease_hue, Engine::increase_hue),
            Key::RgbVai => (Engine::increase_val, Engine::decrease_val),
            Key::RgbVad => (Engine::decrease_val, Engine::increase_val),
            Key::RgbSpi => (Engine::increase_speed, Engine::decrease_speed),
            Key::RgbSpd => (Engine::decrease_speed, Engine::increase_speed),
            _ => return Outcome::Pass,
        };
        if shifted {
            down(e)
        } else {
            up(e)
        }
        Outcome::Handled
    }

    /// One main-loop pass at `now` ms: rgblight_task then rgb_matrix_task.
    /// True when the LEDs changed and should be sent (`engine.leds`).
    pub fn task(&mut self, now: u32) -> bool {
        self.vendor.now = now;
        self.vendor.blink_hook();
        self.vendor.rgblight.task(now);
        let mut changed = self.engine.task(now, &mut self.vendor);
        // With no effect running the indicators don't, so the 0% timer
        // runs here.
        if self.engine.suspended || !self.engine.config.enable {
            self.vendor.bat_stop();
        }
        changed |= self.lights_off_flash();
        changed
    }

    /// The LED chain's supply (A8): on for the lighting, or for the
    /// lights-off battery flash; never while asleep.
    pub fn led_power(&self) -> bool {
        !self.engine.suspended && (self.vendor.power || self.vendor.flash_power)
    }

    /// The keyboard sleeping, or the host suspended (rgb_matrix's
    /// suspend: black at once, then nothing until it wakes). True when
    /// that changed the LEDs (`engine.leds`, to send).
    pub fn set_sleep(&mut self, on: bool, now: u32) -> bool {
        self.vendor.now = now;
        self.engine.set_suspended(on, &mut self.vendor)
    }

    /// The lights-off low-battery flash; true when it changed the LEDs.
    fn lights_off_flash(&mut self) -> bool {
        let v = &mut self.vendor;
        let low = !v.charging && v.bat <= BAT_LOW && !v.led_status && v.bat_enabled;
        if !(v.bat_flash && low && !v.power && !self.engine.suspended) {
            v.flash_since = 0;
            v.flash_power = false;
            return false;
        }
        if v.flash_since == 0 {
            v.flash_since = v.now | 1;
        }
        let t = v.now.wrapping_sub(v.flash_since) % FLASH_EVERY;
        v.flash_power = t < FLASH_END;
        let leds = &mut self.engine.leds;
        let pending = core::mem::replace(&mut leds.dirty, false);
        if (FLASH_ON..FLASH_OFF).contains(&t) {
            leds.set_all(Rgb::OFF);
            leds.set(LED_BAT, RED);
        } else {
            leds.set(LED_BAT, Rgb::OFF);
        }
        let changed = leds.dirty;
        leds.dirty |= pending;
        changed
    }

    /// As `task`, after running every side-light step due by `now`: for
    /// callers that don't run a pass every millisecond.
    pub fn task_sparse(&mut self, now: u32) -> bool {
        self.vendor.rgblight.catch_up(now);
        self.task(now)
    }
}

impl Vendor {
    /// eeconfig_confinfo_default, lighting part: an erased EEPROM.
    fn confinfo_default(&mut self) {
        self.conf = ConfInfo::default();
        self.colours = [0; MODES.len()];
        if let Some(d) = self.default_channels {
            self.channels = *d;
        }
        self.rgblight.mode(SIDE_MODES[0], self.now);
    }

    /// keyboard_post_init_kb, lighting part, after confinfo is loaded.
    fn post_init(&mut self, cfg: &mut Config) {
        // rgbrec_init
        self.recording = false;
        self.rec_channel = self.conf.record_channel;
        self.rec_value = cfg.hsv.v;
        self.read_channel(self.rec_channel);
        // The caller disables the matrix if both are off.
        self.power = self.rgblight.is_enabled() || self.conf.rgb_enable;
        if !self.rgblight.is_enabled() {
            self.side_index = 10;
        }
    }

    /// hs_reset_settings, lighting part (EE_CLR held 3 s).
    fn reset_settings(&mut self, cfg: &mut Config) {
        self.bat_enabled = false;
        *cfg = Config::default();
        // rgblight_init again; the animations' state lives on, as the C's
        // statics do.
        self.rgblight.init(None, self.now);
        self.blink(
            0xFF,
            Rgb {
                r: 0x10,
                g: 0x10,
                b: 0x10,
            },
            250,
            3,
        );
        self.confinfo_default();
        self.post_init(cfg);
        self.reset_requested = true;
    }

    fn blink_dir(&mut self) {
        self.blink(0xFF, Rgb::OFF, 250, 1);
    }

    /// rgb_matrix_hs_indicator_set.
    fn blink(&mut self, index: u8, rgb: Rgb, interval: u16, times: u8) {
        let Some(slot) = self.blinks.iter().position(|b| !b.active) else {
            return;
        };
        self.blinks[slot] = Blink {
            active: true,
            timer: self.now,
            interval,
            times: times * 2,
            index,
            rgb,
        };
    }

    /// rgb_matrix_hs_indicator: run and show the blinks.
    fn show_blinks(&mut self, leds: &mut Leds) {
        for i in 0..BLINKS {
            let b = &mut self.blinks[i];
            if !b.active {
                continue;
            }
            if self.now.wrapping_sub(b.timer) >= b.interval as u32 {
                b.timer = self.now;
                b.times = b.times.saturating_sub(1);
                if b.times == 0 {
                    b.active = false;
                    b.timer = 0;
                    let (index, rgb) = (b.index, b.rgb);
                    if index == 0xFF {
                        self.nkr_hook(rgb);
                    }
                    continue;
                }
            }
            let c = if b.times % 2 == 1 { b.rgb } else { Rgb::OFF };
            if b.index == 0xFF {
                leds.set_all(c);
            } else {
                leds.set(b.index as usize, c);
            }
        }
    }

    /// nkr_indicators_hook: a red whole-board blink is followed by a blue
    /// one, and that by a green one (the C's "red", in QMK's g r b order).
    fn nkr_hook(&mut self, rgb: Rgb) {
        if rgb
            == (Rgb {
                r: 0x6E,
                g: 0,
                b: 0,
            })
        {
            self.blink(
                0xFF,
                Rgb {
                    r: 0,
                    g: 0,
                    b: 0x6E,
                },
                250,
                1,
            );
        } else if rgb
            == (Rgb {
                r: 0,
                g: 0,
                b: 0x6E,
            })
        {
            self.blink(
                0xFF,
                Rgb {
                    r: 0,
                    g: 0x6E,
                    b: 0,
                },
                250,
                1,
            );
        }
    }

    /// rgb_matrix_hs_devs_indicator: on USB, blinks the USB key white
    /// until USB is configured, then shows it for 2 s; on a wireless device,
    /// shows its key for 2 s after connecting.
    fn show_devs(&mut self, leds: &mut Leds) {
        if self.devs == DEVS_USB {
            if !self.usb_active {
                self.devs_timer = self.now;
                if self.usb_blink_timer == 0 || self.now.wrapping_sub(self.usb_blink_timer) > 500 {
                    self.usb_blink_timer = self.now;
                    self.usb_blink_state = !self.usb_blink_state;
                }
                let c = if self.usb_blink_state { 0xFF } else { 0 };
                leds.set(LED_USB, Rgb { r: c, g: c, b: c });
                return;
            }
        } else if !self.devs_flag {
            return;
        }
        self.usb_blink_timer = 0;
        self.usb_blink_state = false;
        if self.now.wrapping_sub(self.devs_timer) >= 2000 {
            self.devs_flag = false;
            return;
        }
        // HS_PAIR_COLOR_*, passed as r, g, b: the colours config.h names.
        let (index, c) = match self.devs {
            DEVS_BT1 => (
                41,
                Rgb {
                    r: 0xFF,
                    g: 0,
                    b: 0,
                },
            ),
            DEVS_BT2 => (
                40,
                Rgb {
                    r: 0,
                    g: 0,
                    b: 0xFF,
                },
            ),
            DEVS_BT3 => (
                39,
                Rgb {
                    r: 0xFF,
                    g: 0xFF,
                    b: 0,
                },
            ),
            DEVS_2G4 => (
                38,
                Rgb {
                    r: 0,
                    g: 0xFF,
                    b: 0,
                },
            ),
            _ => (LED_USB, WHITE),
        };
        leds.set(index, c);
    }

    /// wireless_devs_change, the board's part.
    fn devs_change(&mut self, dev: u8, reset: bool) {
        let old = self.devs;
        if self.devs != dev || reset {
            self.md_state = MD_STATE_DISCONNECTED;
        }
        self.devs = dev;
        self.devs_change_kb(old, dev, reset);
    }

    /// wireless_devs_change_kb: re-pairing blinks the device's key fast in
    /// its LBACK colour, reconnecting slowly in its PAIR colour (both once;
    /// the indicator restarts it while the link stays down).
    fn devs_change_kb(&mut self, _old: u8, new: u8, reset: bool) {
        self.wls.reset = reset;
        if let Some(&(index, lback, pair)) = WLS_BLINKS.get(new as usize) {
            if reset {
                self.wls_set(index, lback, 200, 1);
            } else {
                self.wls_set(index, pair, 500, 1);
            }
        }
    }

    /// rgb_matrix_wls_indicator_set.
    fn wls_set(&mut self, index: u8, rgb: Rgb, interval: u32, times: u32) {
        self.wls.timer = self.now;
        self.wls.index = index;
        self.wls.interval = interval;
        self.wls.times = times * 2;
        self.wls.rgb = rgb;
    }

    /// rgb_matrix_wls_indicator_cb, at the end of a blink: blink again
    /// while the link is down (true), unless it's USB and USB is up.
    fn wls_blink_done(&mut self) -> bool {
        if self.md_state != MD_STATE_CONNECTED {
            if !(self.devs == DEVS_USB && self.usb_active) {
                self.devs_change_kb(self.devs, self.devs, self.wls.reset);
            }
            return true;
        }
        false
    }

    /// rgb_matrix_wls_indicator.
    fn show_wls(&mut self, leds: &mut Leds) {
        if self.wls.timer == 0 {
            return;
        }
        if self.now.wrapping_sub(self.wls.timer) >= self.wls.interval {
            self.wls.timer = self.now;
            self.wls.times = self.wls.times.saturating_sub(1);
            if self.wls.times == 0 {
                self.wls.timer = 0;
                if !self.wls_blink_done() {
                    return;
                }
            }
        }
        let c = if self.wls.times % 2 == 1 {
            self.wls.rgb
        } else {
            Rgb::OFF
        };
        leds.set(self.wls.index as usize, c);
    }

    /// hs_rgb_blink_hook (wls.c), less its sleep timeouts (M7): entering
    /// DISCONNECTED starts the device's blink, entering CONNECTED shows the
    /// device for 2 s.
    fn blink_hook(&mut self) {
        if self.last_status != self.md_state {
            self.last_status = self.md_state;
            self.blink_timer = 0;
        }
        match self.md_state {
            MD_STATE_NONE => self.blink_timer = 0,
            MD_STATE_PAIRING => {
                if self.blink_timer == 0 {
                    self.blink_timer = self.now;
                }
            }
            MD_STATE_DISCONNECTED => {
                if self.blink_timer == 0 {
                    self.blink_timer = self.now;
                    self.devs_change_kb(self.devs, self.devs, false);
                }
            }
            MD_STATE_CONNECTED => {
                if self.blink_timer == 0 {
                    self.blink_timer = self.now;
                    self.devs_flag = true;
                    self.devs_timer = self.now;
                }
            }
            _ => {}
        }
    }

    // ---- the battery (5075b_plus.c's bat_indicators, rgb_record.c's query) ----

    /// rgb_matrix_hs_bat_set, for the one slot.
    fn bat_blink_set(&mut self, rgb: Rgb, interval: u16, times: u8) {
        if !self.bat_blink.active {
            self.bat_blink = Blink {
                active: true,
                timer: self.now,
                interval,
                times: times * 2,
                index: LED_BAT as u8,
                rgb,
            };
        }
    }

    /// rgb_matrix_hs_bat.
    fn show_bat_blink(&mut self, leds: &mut Leds) {
        let b = &mut self.bat_blink;
        if !b.active {
            return;
        }
        if self.now.wrapping_sub(b.timer) >= b.interval as u32 {
            b.timer = self.now;
            b.times = b.times.saturating_sub(1);
            if b.times == 0 {
                b.active = false;
                b.timer = 0;
            }
        }
        let c = if b.times % 2 == 1 { b.rgb } else { Rgb::OFF };
        leds.set(b.index as usize, c);
    }

    /// bat_indicators, less its 0% timer (`bat_stop`).
    fn bat_indicators(&mut self, cfg: &Config, leds: &mut Leds) {
        let show = !self.led_status && cfg.hsv.v != 0;
        if !self.charging && self.bat <= BAT_LOW && show {
            self.bat_blink_set(RED, 250, 1);
        } else {
            leds.set(LED_BAT, Rgb::OFF);
        }
        self.bat_stop();
    }

    /// bat_indicators' 0% timer: a minute at BAT_STOP asks for sleep.
    fn bat_stop(&mut self) {
        if self.charging || self.bat > BAT_LOW {
            self.bat_stop_timer = 0;
        } else if self.bat <= BAT_STOP {
            if self.bat_stop_timer == 0 {
                self.bat_stop_timer = self.now;
            }
            if self.bat_stop_timer != 0 && self.now.wrapping_sub(self.bat_stop_timer) > BAT_STOP_MS
            {
                self.bat_stop_timer = 0;
                self.shutdown_requested = true;
            }
        }
    }

    /// query: while Fn+Space is held, the keys go dark but for a bar of
    /// the level, a LED per 10% (at least one).
    fn query(&self, leds: &mut Leds) {
        if !self.bat_req {
            return;
        }
        for i in 0..SIDE_FIRST {
            leds.set(i, Rgb::OFF);
        }
        let c = match self.bat {
            50.. => Rgb {
                r: 0,
                g: BAT_VAL,
                b: 0,
            },
            30.. => Rgb {
                r: BAT_VAL,
                g: BAT_VAL,
                b: 0,
            },
            _ => Rgb {
                r: BAT_VAL,
                g: 0,
                b: 0,
            },
        };
        for (i, &led) in BAT_BAR.iter().enumerate() {
            let lit = i < (self.bat / 10) as usize || i < 1;
            leds.set(led as usize, if lit { c } else { Rgb::OFF });
        }
    }

    /// find_index: where the effect is in MODES (0 if it isn't).
    fn mode_index(mode: u8) -> usize {
        MODES.iter().position(|&m| m == mode).unwrap_or(0)
    }

    /// record_color_read_data.
    fn colour(&self, mode: u8) -> usize {
        let c = self.colours[Self::mode_index(mode)] as usize;
        if c >= HSVS.len() { 0 } else { c }
    }

    /// record_color_hsv: the next (or previous) colour for this effect;
    /// false at the end of the list.
    fn colour_step(&mut self, e: &mut Engine, up: bool) -> bool {
        let mut c = self.colour(e.config.mode);
        if up {
            if c == HSVS.len() - 1 {
                return false;
            }
            c += 1;
        } else {
            if c == 0 {
                return false;
            }
            c -= 1;
        }
        let (h, s) = HSVS[c];
        e.sethsv(h, s, e.config.hsv.v);
        self.colours[Self::mode_index(e.config.mode)] = c as u8;
        true
    }

    /// record_rgbmatrix_increase: the next effect in MODES, in its colour.
    fn mode_next(&mut self, e: &mut Engine) {
        let mut i = Self::mode_index(e.config.mode);
        if !self.recording {
            i = (i + 1) % MODES.len();
        }
        self.conf.record_last_mode = MODES[i];
        e.set_mode(MODES[i]);
        let (h, s) = HSVS[self.colour(e.config.mode)];
        e.sethsv(h, s, e.config.hsv.v);
    }

    /// im_rgblight_increase: RL_MOD.
    fn side_next(&mut self, now: u32) {
        let l = &mut self.rgblight;
        if l.get_mode() == rgblight::MODE_STATIC {
            // Work out which static colour it's on.
            let hsv = l.config.hsv;
            self.side_index = if hsv.h == 0 && hsv.s != 0 { 3 } else { 9 };
            if let Some(i) = SIDE_HUES[1..].iter().position(|&h| h == hsv.h) {
                self.side_index = i as u8 + 4;
            }
        }
        self.side_index += 1;
        if self.side_index == 11 {
            self.side_index = 0;
        }
        let i = self.side_index as usize;
        if i == 10 {
            let v = l.config.hsv.v;
            l.sethsv(0, 255, v);
            l.disable();
        } else {
            l.enable(now);
            l.mode(SIDE_MODES[i], now);
        }
        let v = l.config.hsv.v;
        match i {
            3..=8 => l.sethsv(SIDE_HUES[i - 3], 255, v),
            9 => l.sethsv(0, 0, v),
            0 => l.set_speed(255),
            _ => l.set_speed(200),
        }
    }

    // ---- rgb_record.c ----

    /// rgbrec_read_current_channel.
    fn read_channel(&mut self, ch: u8) {
        if let Some(c) = self.channels.get(ch as usize) {
            self.rec = *c;
        }
    }

    /// rgbrec_show: play channel `ch`.
    fn rec_show(&mut self, e: &mut Engine, ch: u8) {
        self.rec_channel = ch;
        self.read_channel(ch);
        if e.config.mode != effect::RGBR_PLAY {
            e.set_mode(effect::RGBR_PLAY);
        }
    }

    /// rgbrec_start.
    fn rec_start(&mut self, e: &mut Engine, ch: u8) {
        if ch as usize >= CHANNELS || self.recording {
            return;
        }
        self.recording = true;
        self.rec_channel = ch;
        self.read_channel(ch);
        if e.config.mode != effect::RGBR_PLAY {
            e.set_mode(effect::RGBR_PLAY);
        }
    }

    /// rgbrec_end: stops recording `ch` and saves it.
    fn rec_end(&mut self, ch: u8) {
        if self.recording && ch == self.rec_channel {
            self.recording = false;
            self.channels[ch as usize] = self.rec;
        }
    }

    /// cycle_rgb_next_color: the key's next colour.
    fn record(&mut self, row: u8, col: u8) {
        if !self.recording || row as usize >= MATRIX_ROWS || col as usize >= MATRIX_COLS {
            return;
        }
        let k = &mut self.rec[row as usize * MATRIX_COLS + col as usize];
        *k = match RECORD_COLOURS.iter().position(|&c| c == *k) {
            Some(i) => RECORD_COLOURS[(i + 1) % RECORD_COLOURS.len()],
            None => RECORD_COLOURS[0],
        };
    }
}

impl Hooks for Vendor {
    /// rgb_matrix_indicators_advanced_kb.
    fn indicators_advanced(&mut self, cfg: &mut Config, leds: &mut Leds, _min: u8, _max: u8) {
        if self.ee_clr_timer != 0 && self.now.wrapping_sub(self.ee_clr_timer) > 3000 {
            self.reset_settings(cfg);
            self.ee_clr_timer = 0;
        }
        for (i, &c) in self.rgblight.led.iter().enumerate() {
            leds.set(SIDE_FIRST + i, c);
        }
        self.show_wls(leds);
        if self.bat_enabled {
            self.show_bat_blink(leds);
            self.bat_indicators(cfg, leds);
            self.bat_cnt = self.now;
        } else if self.now.wrapping_sub(self.bat_cnt) > 2000 {
            self.bat_enabled = true;
            self.bat_cnt = self.now;
        }
        self.show_blinks(leds);
        self.show_devs(leds);
        self.query(leds);
        if self.factory_test {
            if self.now.wrapping_sub(self.factory_timer) > 4000 {
                self.factory_timer = self.now;
                self.factory_step += 1;
            }
            let c = match self.factory_step {
                0 => Rgb { r: 80, g: 0, b: 0 },
                1 => Rgb { r: 0, g: 80, b: 0 },
                2 => Rgb { r: 0, g: 0, b: 80 },
                3 => Rgb {
                    r: 80,
                    g: 80,
                    b: 80,
                },
                _ => {
                    self.factory_test = false;
                    return;
                }
            };
            leds.set_all(c);
        }
    }

    /// rgbrec_play: each key's LED in its recorded colour; LEDs without a
    /// key keep what they had.
    fn custom_effect(&mut self, cfg: &Config, leds: &mut Leds, min: u8, max: u8) {
        self.rec_value = cfg.hsv.v;
        for i in min..max {
            let k = LED_KEY[i as usize];
            if k == 0xFF {
                continue;
            }
            let hs = self.rec[k as usize];
            let (h, s) = ((hs >> 8) as u8, hs as u8);
            let hsv = match (h, s) {
                (0, 0) => Hsv { h: 0, s: 0, v: 0 },
                (1, 1) => Hsv {
                    h: 0,
                    s: 0,
                    v: self.rec_value,
                },
                _ => Hsv {
                    h,
                    s,
                    v: self.rec_value,
                },
            };
            leds.set(i as usize, hsv_to_rgb(hsv));
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    static CHANNELS_0: [[u16; KEYS]; CHANNELS] = [[0; KEYS]; CHANNELS];

    /// A board with the lighting turned off (RGB_TOG), at 1000 ms.
    fn dark_board() -> Board {
        let mut b = Board::new(None, &CHANNELS_0, false, 0);
        b.key(Key::RgbTog, 3, 9, true, false, 1000);
        b.key(Key::RgbTog, 3, 9, false, false, 1030);
        assert!(!b.led_power());
        b
    }

    /// Runs frames every 16 ms over [from, to); per frame: (ms, chain
    /// powered, LED 0 lit, any other LED lit).
    fn frames(b: &mut Board, from: u32, to: u32) -> Vec<(u32, bool, bool, bool)> {
        (from..to)
            .step_by(16)
            .map(|t| {
                b.task_sparse(t);
                let leds = &b.engine.leds.rgb;
                (
                    t,
                    b.led_power(),
                    leds[LED_BAT] != Rgb::OFF,
                    leds[1..].iter().any(|&c| c != Rgb::OFF),
                )
            })
            .collect()
    }

    #[test]
    fn lights_off_low_battery_flashes() {
        let mut b = dark_board();
        frames(&mut b, 1040, 2000);
        b.vendor.bat = 12;
        let f = frames(&mut b, 2000, 12_000);
        let lit: Vec<_> = f.iter().filter(|x| x.2).collect();
        // Two flashes of about 150 ms, the chain powered before and after.
        assert!(lit.iter().all(|x| x.1 && !x.3));
        assert!((16..=22).contains(&lit.len()), "{} lit frames", lit.len());
        let powered = f.iter().filter(|x| x.1).count();
        assert!(
            powered > lit.len() && powered <= 2 * 13,
            "{powered} powered frames"
        );
        for w in f.windows(2) {
            // LED 0 lights only with the chain already on, and goes dark
            // before the chain goes off.
            if w[1].2 && !w[0].2 {
                assert!(w[0].1);
            }
            if w[0].2 && !w[1].2 {
                assert!(w[1].1);
            }
        }
    }

    #[test]
    fn no_flash_when_charging_hidden_or_fine() {
        for (bat, charging, hidden) in [(12, true, false), (12, false, true), (40, false, false)] {
            let mut b = dark_board();
            b.vendor.bat = bat;
            b.vendor.charging = charging;
            b.vendor.led_status = hidden;
            assert!(frames(&mut b, 1040, 12_000).iter().all(|x| !x.1 && !x.2));
        }
    }

    #[test]
    fn flat_battery_sleeps_with_the_lights_off() {
        let mut b = dark_board();
        b.vendor.bat = 0;
        frames(&mut b, 1040, 61_000);
        assert!(!b.vendor.shutdown_requested);
        frames(&mut b, 61_000, 62_200);
        assert!(b.vendor.shutdown_requested);
    }

    #[test]
    fn query_with_the_lights_off_powers_the_chain() {
        let mut b = dark_board();
        b.vendor.bat = 35;
        b.key(Key::BatQ, 5, 6, true, false, 1100);
        assert!(b.led_power());
        let f = frames(&mut b, 1100, 1400);
        assert!(f.last().unwrap().3);
        let leds = &b.engine.leds.rgb;
        let yellow = Rgb {
            r: BAT_VAL,
            g: BAT_VAL,
            b: 0,
        };
        assert_eq!(&leds[16..19], &[yellow; 3]);
        assert_eq!(leds[19], Rgb::OFF);
        b.key(Key::BatQ, 5, 6, false, false, 1400);
        assert!(!b.led_power());
    }
}
