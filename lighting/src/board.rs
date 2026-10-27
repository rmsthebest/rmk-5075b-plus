//! The board's own lighting behaviour, from the vendor's QMK code
//! (keyboards/akko/5075b_plus: 5075b_plus.c and rgb_record/): what its RGB
//! keys do around rgb_matrix's, the side lights (`Rgblight`), the
//! whole-board blinks that confirm a key, and the recorded per-key colours
//! that effect 45 (RGBR_PLAY) plays.
//!
//! `Board::key` is process_record_kb (with process_record_hs) followed by
//! QMK's process_rgb for the RGB keys; `Board::task` is a main-loop pass's
//! rgblight_task + rgb_matrix_task, with the board's indicator hooks.
//! Wireless (M6) and battery (M7) indicators aren't here yet.
//!
//! Where the vendor's EEPROM held something (the recorded colour channels,
//! the colour picked for each effect, confinfo), this holds it in RAM; the
//! firmware saves it.

use core::mem::MaybeUninit;

use crate::color::{Hsv, Rgb, hsv_to_rgb};
use crate::effects::effect;
use crate::engine::{Config, Engine, Hooks, Leds, MAX_BRIGHTNESS, VAL_STEP};
use crate::layout::{LED_COUNT, MATRIX_CO, MATRIX_COLS, MATRIX_ROWS, NO_LED};
use crate::rgblight::{self, Rgblight};
use crate::record::Record;

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
    /// KC_ENCW: flips `led_status` (the battery display's mode, M7).
    Encw,
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
    /// KC_ENCW's flag.
    pub led_status: bool,
    /// LED_POWER_EN_PIN (A8), as the vendor's code last set it.
    pub power: bool,
    /// USB is configured (USB_DRIVER.state == USB_ACTIVE).
    pub usb_active: bool,
    usb_blink_timer: u32,
    usb_blink_state: bool,
    devs_timer: u32,
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
        b.engine.init_zeroed(saved.map_or_else(Config::default, Record::config));
        let v = &mut b.vendor;
        v.rgblight.init_zeroed();
        v.conf = ConfInfo::default();
        v.default_channels = Some(default_channels);
        v.channels = *default_channels;
        v.usb_active = true;
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

        // process_record_hs: recording takes every key but these.
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
        self.vendor.rgblight.task(now);
        self.engine.task(now, &mut self.vendor)
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

    /// rgb_matrix_hs_devs_indicator, USB only: blinks the USB key white
    /// until USB is configured, then shows it for 2 s.
    fn show_usb(&mut self, leds: &mut Leds) {
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
        self.usb_blink_timer = 0;
        self.usb_blink_state = false;
        if self.now.wrapping_sub(self.devs_timer) >= 2000 {
            return;
        }
        leds.set(
            LED_USB,
            Rgb {
                r: 0xFF,
                g: 0xFF,
                b: 0xFF,
            },
        );
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
        self.show_blinks(leds);
        self.show_usb(leds);
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
