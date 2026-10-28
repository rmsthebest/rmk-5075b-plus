//! QMK's rgblight.c for the board's 22 side lights, with the board's own
//! driver and effects (rgb_record/rgb_rgblight.c, rgb_effect.c): only what
//! the board's keys reach. RL_MOD steps through static colours (mode 1),
//! breathing (2), rainbow mood (8) and rainbow swirl (14), and the default
//! is swirl (9); the other modes QMK numbers (snake to twinkle, 15-41) keep
//! their numbers but render nothing here.
//!
//! The side lights show through the rgb_matrix: the board's indicator hook
//! copies `led` over LEDs 84-105 every chunk.

use crate::color::{Hsv, Rgb, hsv_to_rgb};

pub const LED_COUNT: usize = 22;

pub const MODE_STATIC: u8 = 1;
pub const MODE_BREATHING: u8 = 2;
pub const MODE_RAINBOW_MOOD: u8 = 6;
pub const MODE_RAINBOW_SWIRL: u8 = 9;
/// RGBLIGHT_MODES with the board's animations.
pub const MODES: u8 = 41;

/// BREATHING_VAL and RAINBOW_MOOD (config.h): the effects' peak level.
const BREATHING_VAL: u8 = 90;
const RAINBOW_MOOD: u8 = 90;
const BREATHING_INTERVALS: [u16; 4] = [30, 20, 10, 5];
const RAINBOW_MOOD_INTERVALS: [u16; 3] = [120, 60, 30];
const RAINBOW_SWIRL_INTERVALS: [u16; 3] = [100, 50, 20];
const RAINBOW_SWIRL_RANGE: u8 = 255;

/// rgblight_config_t.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub enable: bool,
    pub mode: u8,
    pub hsv: Hsv,
    pub speed: u8,
}

impl Default for Config {
    /// eeconfig_update_rgblight_default with the board's config.h.
    fn default() -> Self {
        Config {
            enable: true,
            mode: MODE_RAINBOW_SWIRL,
            hsv: Hsv {
                h: 0,
                s: 255,
                v: 120,
            },
            speed: 0,
        }
    }
}

/// mode_base_table: the first mode of `mode`'s family.
fn base_mode(mode: u8) -> u8 {
    match mode {
        2..=5 => MODE_BREATHING,
        6..=8 => MODE_RAINBOW_MOOD,
        9..=14 => MODE_RAINBOW_SWIRL,
        15..=20 => 15,
        21..=23 => 21,
        24 => 24,
        25..=34 => 25,
        35 => 35,
        36..=41 => 36,
        m => m,
    }
}

/// static_effect_table: static light and static gradient.
fn is_static(mode: u8) -> bool {
    mode == MODE_STATIC || (25..=34).contains(&mode)
}

/// QMK's timer_expired on the 16-bit timer.
fn expired(now: u16, future: u16) -> bool {
    now.wrapping_sub(future) < u16::MAX / 2
}

pub struct Rgblight {
    pub config: Config,
    /// The board's `led` array, which the indicator hook shows.
    pub led: [Rgb; LED_COUNT],
    base_mode: u8,
    timer_enabled: bool,
    restart: bool,
    last_timer: u16,
    current_hue: u8,
    /// rgb_effect.c's statics: breathing (step, r, g, b, dir) and mood.
    breathing: [u8; 5],
    mood: [u8; 4],
}

impl Rgblight {
    /// `Rgblight::new`'s state on an all-zero Rgblight, in place.
    pub(crate) fn init_zeroed(&mut self) {
        self.mood = [0, RAINBOW_MOOD, 0, 0];
    }

    pub const fn new() -> Self {
        Rgblight {
            config: Config {
                enable: false,
                mode: 0,
                hsv: Hsv { h: 0, s: 0, v: 0 },
                speed: 0,
            },
            led: [Rgb::OFF; LED_COUNT],
            base_mode: 0,
            timer_enabled: false,
            restart: false,
            last_timer: 0,
            current_hue: 0,
            breathing: [0; 5],
            mood: [0, RAINBOW_MOOD, 0, 0],
        }
    }

    /// rgblight_init from a saved config (`None`, or mode 0, gives the
    /// defaults, as an erased EEPROM does).
    pub fn init(&mut self, saved: Option<Config>, now: u32) {
        self.config = match saved {
            Some(c) if c.mode != 0 => c,
            _ => Config::default(),
        };
        self.check_config();
        self.timer_enabled = false;
        if self.config.enable {
            self.mode(self.config.mode, now);
        }
    }

    fn check_config(&mut self) {
        self.config.mode = self.config.mode.clamp(MODE_STATIC, MODES);
    }

    /// rgblight_set with the board's driver: off clears the lights.
    fn set(&mut self) {
        if !self.config.enable {
            self.led = [Rgb::OFF; LED_COUNT];
        }
    }

    fn setrgb(&mut self, c: Rgb) {
        if !self.config.enable {
            return;
        }
        self.led = [c; LED_COUNT];
        self.set();
    }

    fn setrgb_all_at(&mut self, c: Rgb) {
        // rgb_effect.c sets each light with rgblight_setrgb_at, which
        // returns while off.
        if self.config.enable {
            self.led = [c; LED_COUNT];
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enable
    }

    /// rgblight_get_mode: 0 while off.
    pub fn get_mode(&self) -> u8 {
        if self.config.enable {
            self.config.mode
        } else {
            0
        }
    }

    /// rgblight_mode_eeprom_helper.
    pub fn mode(&mut self, mode: u8, now: u32) {
        if !self.config.enable {
            return;
        }
        self.config.mode = mode.clamp(MODE_STATIC, MODES);
        if is_static(self.config.mode) {
            self.timer_enabled = false;
        } else {
            self.timer_enabled = true;
            self.last_timer = now as u16;
        }
        self.restart = true;
        let h = self.config.hsv;
        self.sethsv(h.h, h.s, h.v);
    }

    pub fn enable(&mut self, now: u32) {
        self.config.enable = true;
        self.mode(self.config.mode, now);
    }

    pub fn disable(&mut self) {
        self.config.enable = false;
        self.timer_enabled = false;
        self.set();
    }

    /// rgblight_sethsv_eeprom_helper.
    pub fn sethsv(&mut self, mut h: u8, s: u8, mut v: u8) {
        if !self.config.enable {
            return;
        }
        self.base_mode = base_mode(self.config.mode);
        if self.config.mode == MODE_STATIC {
            self.setrgb(hsv_to_rgb(Hsv { h, s, v }));
        } else {
            match self.base_mode {
                MODE_BREATHING => v = self.config.hsv.v,
                MODE_RAINBOW_MOOD | MODE_RAINBOW_SWIRL => h = self.config.hsv.h,
                _ => {}
            }
        }
        self.config.hsv = Hsv { h, s, v };
    }

    pub fn set_speed(&mut self, speed: u8) {
        self.config.speed = speed;
    }

    /// rgblight_timer_task: at most one animation step a call.
    pub fn task(&mut self, now: u32) {
        if !self.timer_enabled {
            return;
        }
        let delta = self.config.mode.wrapping_sub(self.base_mode);
        let interval = match self.base_mode {
            MODE_BREATHING => BREATHING_INTERVALS[delta as usize],
            MODE_RAINBOW_MOOD => RAINBOW_MOOD_INTERVALS[delta as usize],
            MODE_RAINBOW_SWIRL => RAINBOW_SWIRL_INTERVALS[delta as usize / 2],
            _ => 2000,
        };
        let now = now as u16;
        if self.restart {
            self.restart = false;
            self.last_timer = now;
            // pos16, which the swirl's hue shares a union with.
            self.current_hue = 0;
        }
        if expired(now, self.last_timer) {
            self.last_timer = self.last_timer.wrapping_add(interval);
            match self.base_mode {
                MODE_BREATHING => self.breathing(),
                MODE_RAINBOW_MOOD => self.rainbow_mood(),
                MODE_RAINBOW_SWIRL => self.rainbow_swirl(delta),
                _ => {}
            }
        }
    }

    /// Runs every animation step due by `now`, as QMK's main loop would
    /// have by then.
    pub fn catch_up(&mut self, now: u32) {
        while self.timer_enabled && (self.restart || expired(now as u16, self.last_timer)) {
            self.task(now);
        }
    }

    /// rgb_effect.c's breathing: red, green, blue, then red+blue, red+green,
    /// green+blue and white, each up to BREATHING_VAL and down in steps of 2.
    fn breathing(&mut self) {
        let [step, r, g, b, dir] = &mut self.breathing;
        // The channels that move in each step; the first decides the turns.
        let moving: &[usize] = match *step {
            0 => &[0],
            1 => &[1],
            2 => &[2],
            3 => &[0, 2],
            4 => &[0, 1],
            5 => &[1, 2],
            6 => &[1, 2, 0],
            _ => &[],
        };
        if let Some(&lead) = moving.first() {
            let mut ch = [*r, *g, *b];
            if *dir == 0 {
                let n = if ch[lead] < BREATHING_VAL - 1 { 2 } else { 1 };
                for &c in moving {
                    ch[c] = ch[c].wrapping_add(n);
                }
            }
            if ch[lead] == BREATHING_VAL {
                *dir = 1;
            }
            if *dir == 1 {
                let n = if ch[lead] > 1 { 2 } else { 1 };
                for &c in moving {
                    ch[c] = ch[c].wrapping_sub(n);
                }
                if ch[lead] == 0 {
                    *dir = 0;
                    *step = (*step + 1) % 7;
                }
            }
            [*r, *g, *b] = ch;
        }
        let c = Rgb {
            r: *r,
            g: *g,
            b: *b,
        };
        self.setrgb_all_at(c);
    }

    /// rgb_effect.c's rainbow mood: shows the colour, then crossfades one
    /// step red to green to blue to red.
    fn rainbow_mood(&mut self) {
        if !self.config.enable {
            return;
        }
        let [i, r, g, b] = self.mood;
        self.setrgb_all_at(Rgb { r, g, b });
        let mut ch = [r, g, b];
        let mut i = i;
        // Phase i fades channel i down and channel i+1 up.
        for phase in 0..3u8 {
            if i == phase {
                let (down, up) = (phase as usize, (phase as usize + 1) % 3);
                if ch[up] < RAINBOW_MOOD - 1 {
                    ch[down] = ch[down].wrapping_sub(2);
                    ch[up] = ch[up].wrapping_add(2);
                } else {
                    ch[down] = ch[down].wrapping_sub(1);
                    ch[up] = ch[up].wrapping_add(1);
                }
                if ch[up] == RAINBOW_MOOD {
                    i = (phase + 1) % 3;
                }
            }
        }
        self.mood = [i, ch[0], ch[1], ch[2]];
    }

    /// rgb_effect.c's rainbow swirl: mirrored about the middle, 3 hue steps
    /// a tick.
    fn rainbow_swirl(&mut self, delta: u8) {
        let n = LED_COUNT as u8;
        let Hsv { s, v, .. } = self.config.hsv;
        for i in 0..n / 2 {
            let h = (RAINBOW_SWIRL_RANGE / n)
                .wrapping_mul(i)
                .wrapping_add(self.current_hue);
            let c = hsv_to_rgb(Hsv { h, s, v });
            self.led[i as usize] = c;
            self.led[(n - 1 - i) as usize] = c;
        }
        self.set();
        self.current_hue = if delta % 2 == 1 {
            self.current_hue.wrapping_add(3)
        } else {
            self.current_hue.wrapping_sub(3)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The firmware runs the side lights only at frame times; `catch_up`
    /// must leave them as a task every millisecond would.
    #[test]
    fn catch_up_matches_every_ms() {
        for mode in [2, 3, 5, 6, 8, 9, 14] {
            let (mut dense, mut sparse) = (Rgblight::new(), Rgblight::new());
            dense.init(None, 0);
            sparse.init(None, 0);
            dense.mode(mode, 0);
            sparse.mode(mode, 0);
            for t in 0..20_000u32 {
                dense.task(t);
                if t % 16 == 0 {
                    sparse.catch_up(t);
                    assert_eq!(dense.led, sparse.led, "mode {mode} at {t} ms");
                }
            }
        }
    }
}
