//! quantum/rgb_matrix/animations: the runners and every effect the board
//! enables, in QMK's order. Casts follow the C: `as u8` / `as i8` where the
//! C stores into a uint8_t / int8_t (or passes one), i32 where it computes
//! in int. The board leaves RGB_MATRIX_SOLID_REACTIVE_GRADIENT_MODE and
//! RGB_MATRIX_TYPING_HEATMAP_SLIM off.

use crate::color::{Hsv, Rgb, hsv_to_rgb};
use crate::engine::{Engine, HITS, Hooks, LED_FLAG_MODIFIER, limits, more_after};
use crate::layout::{CENTER, LED_COUNT, MATRIX_CO, MATRIX_COLS, MATRIX_ROWS, NO_LED, POINTS};
use crate::math::{abs8, atan2_8, cos8, qadd8, qsub8, scale8, scale16by8, sin8, sqrt16};

/// Effect numbers: QMK's enum rgb_matrix_effects for this board's
/// keyboard.json, which is also what VIA and the EEPROM store.
pub mod effect {
    pub const NONE: u8 = 0;
    pub const SOLID_COLOR: u8 = 1;
    pub const ALPHAS_MODS: u8 = 2;
    pub const GRADIENT_UP_DOWN: u8 = 3;
    pub const GRADIENT_LEFT_RIGHT: u8 = 4;
    pub const BREATHING: u8 = 5;
    pub const BAND_SAT: u8 = 6;
    pub const BAND_VAL: u8 = 7;
    pub const BAND_PINWHEEL_SAT: u8 = 8;
    pub const BAND_PINWHEEL_VAL: u8 = 9;
    pub const BAND_SPIRAL_SAT: u8 = 10;
    pub const BAND_SPIRAL_VAL: u8 = 11;
    pub const CYCLE_ALL: u8 = 12;
    pub const CYCLE_LEFT_RIGHT: u8 = 13;
    pub const CYCLE_UP_DOWN: u8 = 14;
    pub const RAINBOW_MOVING_CHEVRON: u8 = 15;
    pub const CYCLE_OUT_IN: u8 = 16;
    pub const CYCLE_OUT_IN_DUAL: u8 = 17;
    pub const CYCLE_PINWHEEL: u8 = 18;
    pub const CYCLE_SPIRAL: u8 = 19;
    pub const DUAL_BEACON: u8 = 20;
    pub const RAINBOW_BEACON: u8 = 21;
    pub const RAINBOW_PINWHEELS: u8 = 22;
    pub const RAINDROPS: u8 = 23;
    pub const JELLYBEAN_RAINDROPS: u8 = 24;
    pub const HUE_BREATHING: u8 = 25;
    pub const HUE_PENDULUM: u8 = 26;
    pub const HUE_WAVE: u8 = 27;
    pub const PIXEL_RAIN: u8 = 28;
    pub const PIXEL_FLOW: u8 = 29;
    pub const PIXEL_FRACTAL: u8 = 30;
    pub const TYPING_HEATMAP: u8 = 31;
    pub const DIGITAL_RAIN: u8 = 32;
    pub const SOLID_REACTIVE_SIMPLE: u8 = 33;
    pub const SOLID_REACTIVE: u8 = 34;
    pub const SOLID_REACTIVE_WIDE: u8 = 35;
    pub const SOLID_REACTIVE_MULTIWIDE: u8 = 36;
    pub const SOLID_REACTIVE_CROSS: u8 = 37;
    pub const SOLID_REACTIVE_MULTICROSS: u8 = 38;
    pub const SOLID_REACTIVE_NEXUS: u8 = 39;
    pub const SOLID_REACTIVE_MULTINEXUS: u8 = 40;
    pub const SPLASH: u8 = 41;
    pub const MULTISPLASH: u8 = 42;
    pub const SOLID_SPLASH: u8 = 43;
    pub const SOLID_MULTISPLASH: u8 = 44;
    /// The board's recorded-colour playback (rgb_matrix_user.inc).
    pub const RGBR_PLAY: u8 = 45;
    /// RGB_MATRIX_EFFECT_MAX.
    pub const COUNT: u8 = 46;
}

/// pixel_fractal's MID_COL.
pub(crate) const MID_COL: usize = MATRIX_COLS / 2;

const HEATMAP_INCREASE_STEP: u8 = 32;
const HEATMAP_DECREASE_DELAY_MS: u16 = 25;
const HEATMAP_SPREAD: u8 = 40;
const HEATMAP_AREA_LIMIT: u8 = 16;
const DIGITAL_RAIN_DROPS: i32 = 24;
const RAND_MAX: i32 = 0x7FFF_FFFF;

type IFn = fn(&Engine, Hsv, u8, u8) -> Hsv;
type DxDyFn = fn(&Engine, Hsv, i16, i16, u8) -> Hsv;
type DxDyDistFn = fn(&Engine, Hsv, i16, i16, u8, u8) -> Hsv;
type SinCosFn = fn(&Engine, Hsv, i8, i8, u8, u8) -> Hsv;
type ReactiveFn = fn(&Engine, Hsv, u16) -> Hsv;
type SplashFn = fn(&Engine, Hsv, i16, i16, u8, u16) -> Hsv;

fn px(i: u8) -> i32 {
    POINTS[i as usize].0 as i32
}

fn py(i: u8) -> i32 {
    POINTS[i as usize].1 as i32
}

const CX: i32 = CENTER.0 as i32;
const CY: i32 = CENTER.1 as i32;

impl Engine {
    pub(crate) fn render(&mut self, effect: u8, hooks: &mut impl Hooks) -> bool {
        use effect::*;
        match effect {
            NONE => {
                if self.params.init {
                    self.leds.set_all(Rgb::OFF);
                }
                false
            }
            SOLID_COLOR => self.solid_color(),
            ALPHAS_MODS => self.alphas_mods(),
            GRADIENT_UP_DOWN => self.gradient_up_down(),
            GRADIENT_LEFT_RIGHT => self.gradient_left_right(),
            BREATHING => self.breathing(),
            BAND_SAT => self.runner_i(band_sat),
            BAND_VAL => self.runner_i(band_val),
            BAND_PINWHEEL_SAT => self.runner_dx_dy(band_pinwheel_sat),
            BAND_PINWHEEL_VAL => self.runner_dx_dy(band_pinwheel_val),
            BAND_SPIRAL_SAT => self.runner_dx_dy_dist(band_spiral_sat),
            BAND_SPIRAL_VAL => self.runner_dx_dy_dist(band_spiral_val),
            CYCLE_ALL => self.runner_i(|_, mut hsv, _, time| {
                hsv.h = time;
                hsv
            }),
            CYCLE_LEFT_RIGHT => self.runner_i(|_, mut hsv, i, time| {
                hsv.h = (px(i) - time as i32) as u8;
                hsv
            }),
            CYCLE_UP_DOWN => self.runner_i(|_, mut hsv, i, time| {
                hsv.h = (py(i) - time as i32) as u8;
                hsv
            }),
            RAINBOW_MOVING_CHEVRON => self.runner_i(|_, mut hsv, i, time| {
                hsv.h =
                    (hsv.h as i32 + abs8((py(i) - CY) as i8) as i32 + (px(i) - time as i32)) as u8;
                hsv
            }),
            CYCLE_OUT_IN => self.runner_dx_dy_dist(|_, mut hsv, _, _, dist, time| {
                hsv.h = (3 * dist as i32 / 2 + time as i32) as u8;
                hsv
            }),
            CYCLE_OUT_IN_DUAL => self.runner_dx_dy(|_, mut hsv, dx, dy, time| {
                let dx = (CX / 2 - abs8(dx as i8) as i32) as i16 as i32;
                let dy = dy as i32;
                let dist = sqrt16((dx * dx + dy * dy) as u16);
                hsv.h = (3 * dist as i32 + time as i32) as u8;
                hsv
            }),
            CYCLE_PINWHEEL => self.runner_dx_dy(|_, mut hsv, dx, dy, time| {
                hsv.h = atan2_8(dy, dx).wrapping_add(time);
                hsv
            }),
            CYCLE_SPIRAL => self.runner_dx_dy_dist(|_, mut hsv, dx, dy, dist, time| {
                hsv.h = (dist as i32 - time as i32 - atan2_8(dy, dx) as i32) as u8;
                hsv
            }),
            DUAL_BEACON => self.runner_sin_cos_i(|_, mut hsv, sin, cos, i, _| {
                let d = ((py(i) - CY) * cos as i32 + (px(i) - CX) * sin as i32) / 128;
                hsv.h = (hsv.h as i32 + d) as u8;
                hsv
            }),
            RAINBOW_BEACON => self.runner_sin_cos_i(|_, mut hsv, sin, cos, i, _| {
                let d = ((py(i) - CY) * 2 * cos as i32 + (px(i) - CX) * 2 * sin as i32) / 128;
                hsv.h = (hsv.h as i32 + d) as u8;
                hsv
            }),
            RAINBOW_PINWHEELS => self.runner_sin_cos_i(|_, mut hsv, sin, cos, i, _| {
                let d = ((py(i) - CY) * 3 * cos as i32
                    + (56 - abs8((px(i) - CX) as i8) as i32) * 3 * sin as i32)
                    / 128;
                hsv.h = (hsv.h as i32 + d) as u8;
                hsv
            }),
            RAINDROPS => self.raindrops(),
            JELLYBEAN_RAINDROPS => self.jellybean_raindrops(),
            HUE_BREATHING => self.hue_breathing(),
            HUE_PENDULUM => self.runner_i(|_, mut hsv, i, time| {
                let a = abs8((sin8(time) as i32 + px(i) - 128) as i8) as i32 * 2;
                hsv.h = hsv.h.wrapping_add(scale8(a as u8, 12));
                hsv
            }),
            HUE_WAVE => self.runner_i(|_, mut hsv, i, time| {
                hsv.h = hsv
                    .h
                    .wrapping_add(scale8(abs8((px(i) - time as i32) as i8) as u8, 24));
                hsv
            }),
            PIXEL_RAIN => self.pixel_rain(),
            PIXEL_FLOW => self.pixel_flow(),
            PIXEL_FRACTAL => self.pixel_fractal(),
            TYPING_HEATMAP => self.typing_heatmap(),
            DIGITAL_RAIN => self.digital_rain(),
            SOLID_REACTIVE_SIMPLE => self.runner_reactive(|_, mut hsv, offset| {
                hsv.v = scale8((255 - offset as i32) as u8, hsv.v);
                hsv
            }),
            SOLID_REACTIVE => self.runner_reactive(|_, mut hsv, offset| {
                hsv.h = hsv.h.wrapping_add(scale8((255 - offset as i32) as u8, 64));
                hsv
            }),
            SOLID_REACTIVE_WIDE => self.runner_splash(qsub8(self.hits.count, 1), reactive_wide),
            SOLID_REACTIVE_MULTIWIDE => self.runner_splash(0, reactive_wide),
            SOLID_REACTIVE_CROSS => self.runner_splash(qsub8(self.hits.count, 1), reactive_cross),
            SOLID_REACTIVE_MULTICROSS => self.runner_splash(0, reactive_cross),
            SOLID_REACTIVE_NEXUS => self.runner_splash(qsub8(self.hits.count, 1), reactive_nexus),
            SOLID_REACTIVE_MULTINEXUS => self.runner_splash(0, reactive_nexus),
            SPLASH => self.runner_splash(qsub8(self.hits.count, 1), splash),
            MULTISPLASH => self.runner_splash(0, splash),
            SOLID_SPLASH => self.runner_splash(qsub8(self.hits.count, 1), solid_splash),
            SOLID_MULTISPLASH => self.runner_splash(0, solid_splash),
            RGBR_PLAY => {
                let (min, max) = limits(self.params.iter);
                hooks.custom_effect(&self.config, &mut self.leds, min, max);
                more_after(max)
            }
            _ => false,
        }
    }

    fn set_hsv(&mut self, i: u8, hsv: Hsv) {
        self.leds.set(i as usize, hsv_to_rgb(hsv));
    }

    /// scale16by8(g_rgb_timer, ...): the C passes the u32 timer as uint16_t.
    fn timer16(&self) -> u16 {
        self.timer as u16
    }

    // ---- runners ----

    fn runner_i(&mut self, f: IFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let time = scale16by8(self.timer16(), qadd8(self.config.speed / 4, 1)) as u8;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let hsv = f(self, self.config.hsv, i, time);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn runner_dx_dy(&mut self, f: DxDyFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let time = scale16by8(self.timer16(), self.config.speed / 2) as u8;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let dx = (px(i) - CX) as i16;
            let dy = (py(i) - CY) as i16;
            let hsv = f(self, self.config.hsv, dx, dy, time);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn runner_dx_dy_dist(&mut self, f: DxDyDistFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let time = scale16by8(self.timer16(), self.config.speed / 2) as u8;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let dx = (px(i) - CX) as i16;
            let dy = (py(i) - CY) as i16;
            let dist = sqrt16((dx as i32 * dx as i32 + dy as i32 * dy as i32) as u16);
            let hsv = f(self, self.config.hsv, dx, dy, dist, time);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    /// Passes (cos, sin) to parameters named (sin, cos), as QMK does.
    fn runner_sin_cos_i(&mut self, f: SinCosFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let time = scale16by8(self.timer16(), self.config.speed / 4);
        let cos_value = (cos8(time as u8) as i32 - 128) as i8;
        let sin_value = (sin8(time as u8) as i32 - 128) as i8;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let hsv = f(self, self.config.hsv, cos_value, sin_value, i, time as u8);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn runner_reactive(&mut self, f: ReactiveFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let max_tick = (65535 / qadd8(self.config.speed, 1) as u32) as u16;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let mut tick = max_tick;
            for j in (0..self.hits.count as usize).rev() {
                if self.hits.index[j] == i && self.hits.tick[j] < tick {
                    tick = self.hits.tick[j];
                    break;
                }
            }
            let offset = scale16by8(tick, qadd8(self.config.speed, 1));
            let hsv = f(self, self.config.hsv, offset);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn runner_splash(&mut self, start: u8, f: SplashFn) -> bool {
        let (min, max) = limits(self.params.iter);
        let count = self.hits.count;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let mut hsv = self.config.hsv;
            hsv.v = 0;
            for j in start as usize..(count as usize).min(HITS) {
                let dx = (px(i) - self.hits.x[j] as i32) as i16;
                let dy = (py(i) - self.hits.y[j] as i32) as i16;
                let dist = sqrt16((dx as i32 * dx as i32 + dy as i32 * dy as i32) as u16);
                let tick = scale16by8(self.hits.tick[j], qadd8(self.config.speed, 1));
                hsv = f(self, hsv, dx, dy, dist, tick);
            }
            hsv.v = scale8(hsv.v, self.config.hsv.v);
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    // ---- effects with their own loops ----

    fn solid_color(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let rgb = hsv_to_rgb(self.config.hsv);
        for i in min..max {
            if self.has_flags(i as usize) {
                self.leds.set(i as usize, rgb);
            }
        }
        more_after(max)
    }

    fn alphas_mods(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let mut hsv = self.config.hsv;
        let rgb1 = hsv_to_rgb(hsv);
        hsv.h = hsv.h.wrapping_add(self.config.speed);
        let rgb2 = hsv_to_rgb(hsv);
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            let modifier =
                crate::layout::FLAGS[i as usize] & LED_FLAG_MODIFIER == LED_FLAG_MODIFIER;
            self.leds
                .set(i as usize, if modifier { rgb2 } else { rgb1 });
        }
        more_after(max)
    }

    fn gradient_up_down(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let mut hsv = self.config.hsv;
        let scale = scale8(64, self.config.speed) as i32;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            hsv.h = (self.config.hsv.h as i32 + scale * (py(i) >> 4)) as u8;
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn gradient_left_right(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let mut hsv = self.config.hsv;
        let scale = scale8(64, self.config.speed) as i32;
        for i in min..max {
            if !self.has_flags(i as usize) {
                continue;
            }
            hsv.h = (self.config.hsv.h as i32 + ((scale * px(i)) >> 5)) as u8;
            self.set_hsv(i, hsv);
        }
        more_after(max)
    }

    fn breathing(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let mut hsv = self.config.hsv;
        let time = scale16by8(self.timer16(), self.config.speed / 8);
        hsv.v = scale8(
            (abs8((sin8(time as u8) as i32 - 128) as i8) as i32 * 2) as u8,
            hsv.v,
        );
        let rgb = hsv_to_rgb(hsv);
        for i in min..max {
            if self.has_flags(i as usize) {
                self.leds.set(i as usize, rgb);
            }
        }
        more_after(max)
    }

    fn hue_breathing(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        let mut hsv = self.config.hsv;
        let time = scale16by8(self.timer16(), self.config.speed / 8);
        let a = abs8((sin8(time as u8) as i32 - 128) as i8) as i32 * 2;
        hsv.h = hsv.h.wrapping_add(scale8(a as u8, 12));
        let rgb = hsv_to_rgb(hsv);
        for i in min..max {
            if self.has_flags(i as usize) {
                self.leds.set(i as usize, rgb);
            }
        }
        more_after(max)
    }

    fn raindrop(&mut self, i: u8) {
        if !self.has_flags(i as usize) {
            return;
        }
        let h = self.config.hsv.h as i32;
        let mut delta_h = ((h + 180) % 360 - h) / 4;
        if delta_h > 127 {
            delta_h -= 256;
        } else if delta_h < -127 {
            delta_h += 256;
        }
        let r = (self.rand.random8() & 0x03) as i32;
        let hsv = Hsv {
            h: (h + delta_h * r) as u8,
            s: self.config.hsv.s,
            v: self.config.hsv.v,
        };
        self.set_hsv(i, hsv);
    }

    fn raindrops(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        if !self.params.init {
            if scale16by8(self.timer16(), self.speed_plus(16)) % 10 == 0 {
                let i = self.rand.random8_max(LED_COUNT as u8);
                self.raindrop(i);
            }
        } else {
            for i in min..max {
                self.raindrop(i);
            }
        }
        more_after(max)
    }

    fn jellybean_drop(&mut self, i: u8) {
        if !self.has_flags(i as usize) {
            return;
        }
        let h = self.rand.random8();
        let s = self.rand.random8_min_max(127, 255);
        self.set_hsv(
            i,
            Hsv {
                h,
                s,
                v: self.config.hsv.v,
            },
        );
    }

    fn jellybean_raindrops(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        if !self.params.init {
            if scale16by8(self.timer16(), self.speed_plus(16)) % 5 == 0 {
                let i = self.rand.random8_max(LED_COUNT as u8);
                self.jellybean_drop(i);
            }
        } else {
            for i in min..max {
                self.jellybean_drop(i);
            }
        }
        more_after(max)
    }

    /// `(random8() & 2) ? off : {random8(), random8_min_max(127, 255), v}`.
    fn random_pixel(&mut self) -> Hsv {
        if self.rand.random8() & 2 != 0 {
            Hsv { h: 0, s: 0, v: 0 }
        } else {
            let h = self.rand.random8();
            let s = self.rand.random8_min_max(127, 255);
            Hsv {
                h,
                s,
                v: self.config.hsv.v,
            }
        }
    }

    /// `n / scale16by8(qadd8(speed, 16), 16)`, the pixel effects' interval.
    fn pixel_interval(&self, n: u32) -> u32 {
        n / scale16by8(self.speed_plus(16) as u16, 16) as u32
    }

    fn pixel_rain(&mut self) -> bool {
        let (_, max) = limits(self.params.iter);
        if self.timer > self.st.pixel_rain_wait {
            let i = self.rand.random8_max(LED_COUNT as u8);
            if self.has_flags(i as usize) {
                let hsv = self.random_pixel();
                self.set_hsv(i, hsv);
                self.st.pixel_rain_wait = self.timer.wrapping_add(self.pixel_interval(500));
            }
        }
        more_after(max)
    }

    fn pixel_flow(&mut self) -> bool {
        if self.st.pixel_flow_wait > self.timer {
            return false;
        }
        if self.params.init {
            self.leds.set_all(Rgb::OFF);
            for j in 0..LED_COUNT {
                let hsv = self.random_pixel();
                self.st.pixel_flow_led[j] = hsv_to_rgb(hsv);
            }
        }
        let (min, max) = limits(self.params.iter);
        for i in min..max {
            if self.has_flags(i as usize) {
                self.leds
                    .set(i as usize, self.st.pixel_flow_led[i as usize]);
            }
        }
        if !more_after(max) {
            let max = max as usize;
            self.st.pixel_flow_led.copy_within(1..max, 0);
            let hsv = self.random_pixel();
            self.st.pixel_flow_led[max - 1] = hsv_to_rgb(hsv);
            self.st.pixel_flow_wait = self.timer.wrapping_add(self.pixel_interval(3000));
        }
        more_after(max)
    }

    fn fractal_set(&mut self, row: usize, col: usize, rgb: Rgb) {
        let i = MATRIX_CO[row][col] as usize;
        if self.has_flags(i) {
            self.leds.set(i, rgb);
        }
    }

    fn pixel_fractal(&mut self) -> bool {
        if self.params.init {
            self.leds.set_all(Rgb::OFF);
        }
        let (_, max) = limits(self.params.iter);
        if self.timer > self.st.fractal_wait {
            // QMK writes `(RGB){rgb.r, rgb.g, rgb.b}` into an RGB whose
            // fields are g, r, b on this GRB board: red and green swap.
            let c = hsv_to_rgb(self.config.hsv);
            let rgb = Rgb {
                r: c.g,
                g: c.r,
                b: c.b,
            };
            for h in 0..MATRIX_ROWS {
                for l in 0..MID_COL - 1 {
                    let c = if self.st.fractal_led[h][l] {
                        rgb
                    } else {
                        Rgb::OFF
                    };
                    self.fractal_set(h, l, c);
                    self.fractal_set(h, MATRIX_COLS - 1 - l, c);
                    self.st.fractal_led[h][l] = self.st.fractal_led[h][l + 1];
                }
                let c = if self.st.fractal_led[h][MID_COL - 1] {
                    rgb
                } else {
                    Rgb::OFF
                };
                self.fractal_set(h, MID_COL - 1, c);
                self.fractal_set(h, MATRIX_COLS - MID_COL, c);
                self.st.fractal_led[h][MID_COL - 1] = self.rand.random8() & 3 == 0;
            }
            self.st.fractal_wait = self.timer.wrapping_add(self.pixel_interval(3000));
        }
        more_after(max)
    }

    /// process_rgb_matrix_typing_heatmap.
    pub(crate) fn heatmap_key(&mut self, row: u8, col: u8) {
        let (row, col) = (row as usize, col as usize);
        let fb = &mut self.frame_buffer;
        let here = MATRIX_CO[row][col];
        if here == NO_LED {
            return;
        }
        for i_row in 0..MATRIX_ROWS {
            for i_col in 0..MATRIX_COLS {
                let there = MATRIX_CO[i_row][i_col];
                if there == NO_LED {
                    continue;
                }
                if i_row == row && i_col == col {
                    fb[row][col] = qadd8(fb[row][col], HEATMAP_INCREASE_STEP);
                } else {
                    let ddx = (px(here) - px(there)) as i16 as i32;
                    let ddy = (py(here) - py(there)) as i16 as i32;
                    let distance = sqrt16((ddx * ddx + ddy * ddy) as u16);
                    if distance <= HEATMAP_SPREAD {
                        let amount = qsub8(HEATMAP_SPREAD, distance).min(HEATMAP_AREA_LIMIT);
                        fb[i_row][i_col] = qadd8(fb[i_row][i_col], amount);
                    }
                }
            }
        }
    }

    fn typing_heatmap(&mut self) -> bool {
        let (min, max) = limits(self.params.iter);
        if self.params.init {
            self.leds.set_all(Rgb::OFF);
            self.frame_buffer = [[0; MATRIX_COLS]; MATRIX_ROWS];
        }
        if self.params.iter == 0 {
            let now16 = self.now as u16;
            self.st.heatmap_decrease =
                now16.wrapping_sub(self.st.heatmap_decrease_timer) >= HEATMAP_DECREASE_DELAY_MS;
            if self.st.heatmap_decrease {
                self.st.heatmap_decrease_timer = now16;
            }
        }
        let limit = crate::engine::PROCESS_LIMIT;
        let mut count: u8 = 0;
        let mut row = 0;
        while row < MATRIX_ROWS && count < limit {
            for col in 0..MATRIX_COLS {
                let i = MATRIX_CO[row][col];
                if i >= min && i < max {
                    count = count.wrapping_add(1);
                    let val = self.frame_buffer[row][col];
                    if !self.has_flags(i as usize) {
                        continue;
                    }
                    let hsv = Hsv {
                        h: (170 - qsub8(val, 85) as i32) as u8,
                        s: self.config.hsv.s,
                        v: scale8(
                            ((qadd8(170, val) as i32 - 170) * 3) as u8,
                            self.config.hsv.v,
                        ),
                    };
                    self.set_hsv(i, hsv);
                    if self.st.heatmap_decrease {
                        self.frame_buffer[row][col] = qsub8(val, 1);
                    }
                }
            }
            row += 1;
        }
        more_after(max)
    }

    /// newlib's rand().
    fn crand(&mut self) -> i32 {
        self.crand = self.crand.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((self.crand >> 32) as i32) & RAND_MAX
    }

    fn digital_rain(&mut self) -> bool {
        // Divisions by zero give 0, as the Cortex-M3's divider does.
        let div = |a: i32, b: i32| if b == 0 { 0 } else { a / b };
        const DROP_TICKS: u8 = 28;
        let v = self.config.hsv.v;
        let pure_green_intensity = ((v as u16 * 3) >> 2) as u8;
        let max_brightness_boost = ((v as u16 * 3) >> 2) as u8;
        let max_intensity = v;
        let decay_ticks = div(0xff, max_intensity as i32) as u8;

        if self.params.init {
            self.leds.set_all(Rgb::OFF);
            self.frame_buffer = [[0; MATRIX_COLS]; MATRIX_ROWS];
            self.st.rain_drop = 0;
        }
        self.st.rain_decay = self.st.rain_decay.wrapping_add(1);
        for col in 0..MATRIX_COLS {
            for row in 0..MATRIX_ROWS {
                if row == 0
                    && self.st.rain_drop == 0
                    && self.crand() < RAND_MAX / DIGITAL_RAIN_DROPS
                {
                    self.frame_buffer[row][col] = max_intensity;
                } else if self.frame_buffer[row][col] > 0
                    && self.frame_buffer[row][col] < max_intensity
                {
                    if self.st.rain_decay == decay_ticks {
                        self.frame_buffer[row][col] -= 1;
                    }
                }
                let led = MATRIX_CO[row][col];
                if led != NO_LED {
                    let fb = self.frame_buffer[row][col] as i32;
                    let pgi = pure_green_intensity as i32;
                    if fb > pgi {
                        let boost = div(
                            max_brightness_boost as i32 * (fb - pgi),
                            max_intensity as i32 - pgi,
                        ) as u8;
                        self.leds.set(
                            led as usize,
                            Rgb {
                                r: boost,
                                g: max_intensity,
                                b: boost,
                            },
                        );
                    } else {
                        let green = div(max_intensity as i32 * fb, pgi) as u8;
                        self.leds.set(
                            led as usize,
                            Rgb {
                                r: 0,
                                g: green,
                                b: 0,
                            },
                        );
                    }
                }
            }
        }
        if self.st.rain_decay == decay_ticks {
            self.st.rain_decay = 0;
        }
        self.st.rain_drop = self.st.rain_drop.wrapping_add(1);
        if self.st.rain_drop > DROP_TICKS {
            self.st.rain_drop = 0;
            for row in (1..MATRIX_ROWS).rev() {
                for col in 0..MATRIX_COLS {
                    if row == MATRIX_ROWS - 1 && self.frame_buffer[row][col] == max_intensity {
                        self.frame_buffer[row][col] = self.frame_buffer[row][col].wrapping_sub(1);
                    }
                    if self.frame_buffer[row - 1][col] >= max_intensity {
                        self.frame_buffer[row - 1][col] = max_intensity.wrapping_sub(1);
                        self.frame_buffer[row][col] = max_intensity;
                    }
                }
            }
        }
        false
    }
}

fn band_sat(_: &Engine, mut hsv: Hsv, i: u8, time: u8) -> Hsv {
    let s = (hsv.s as i32 - (scale8(px(i) as u8, 228) as i32 + 28 - time as i32).abs() * 8) as i16;
    hsv.s = scale8(if s < 0 { 0 } else { s as u8 }, hsv.s);
    hsv
}

fn band_val(_: &Engine, mut hsv: Hsv, i: u8, time: u8) -> Hsv {
    let v = (hsv.v as i32 - (scale8(px(i) as u8, 228) as i32 + 28 - time as i32).abs() * 8) as i16;
    hsv.v = scale8(if v < 0 { 0 } else { v as u8 }, hsv.v);
    hsv
}

fn band_pinwheel_sat(_: &Engine, mut hsv: Hsv, dx: i16, dy: i16, time: u8) -> Hsv {
    hsv.s = scale8(
        (hsv.s as i32 - time as i32 - atan2_8(dy, dx) as i32 * 3) as u8,
        hsv.s,
    );
    hsv
}

fn band_pinwheel_val(_: &Engine, mut hsv: Hsv, dx: i16, dy: i16, time: u8) -> Hsv {
    hsv.v = scale8(
        (hsv.v as i32 - time as i32 - atan2_8(dy, dx) as i32 * 3) as u8,
        hsv.v,
    );
    hsv
}

fn band_spiral_sat(_: &Engine, mut hsv: Hsv, dx: i16, dy: i16, dist: u8, time: u8) -> Hsv {
    hsv.s = scale8(
        (hsv.s as i32 + dist as i32 - time as i32 - atan2_8(dy, dx) as i32) as u8,
        hsv.s,
    );
    hsv
}

fn band_spiral_val(_: &Engine, mut hsv: Hsv, dx: i16, dy: i16, dist: u8, time: u8) -> Hsv {
    hsv.v = scale8(
        (hsv.v as i32 + dist as i32 - time as i32 - atan2_8(dy, dx) as i32) as u8,
        hsv.v,
    );
    hsv
}

fn reactive_wide(_: &Engine, mut hsv: Hsv, _: i16, _: i16, dist: u8, tick: u16) -> Hsv {
    let mut effect = (tick as i32 + dist as i32 * 5) as u16;
    if effect > 255 {
        effect = 255;
    }
    hsv.v = qadd8(hsv.v, (255 - effect as i32) as u8);
    hsv
}

fn reactive_cross(_: &Engine, mut hsv: Hsv, dx: i16, dy: i16, dist: u8, tick: u16) -> Hsv {
    let mut effect = (tick as i32 + dist as i32) as u16;
    let dx = if dx < 0 { (dx as i32 * -1) as i16 } else { dx };
    let dy = if dy < 0 { (dy as i32 * -1) as i16 } else { dy };
    let dx = if dx as i32 * 16 > 255 {
        255
    } else {
        (dx as i32 * 16) as i16
    };
    let dy = if dy as i32 * 16 > 255 {
        255
    } else {
        (dy as i32 * 16) as i16
    };
    effect = (effect as i32 + if dx > dy { dy } else { dx } as i32) as u16;
    if effect > 255 {
        effect = 255;
    }
    hsv.v = qadd8(hsv.v, (255 - effect as i32) as u8);
    hsv
}

fn reactive_nexus(e: &Engine, mut hsv: Hsv, dx: i16, dy: i16, dist: u8, tick: u16) -> Hsv {
    let mut effect = (tick as i32 - dist as i32) as u16;
    if effect > 255 {
        effect = 255;
    }
    if dist > 72 {
        effect = 255;
    }
    if (dx > 8 || dx < -8) && (dy > 8 || dy < -8) {
        effect = 255;
    }
    hsv.h = (e.config.hsv.h as i32 + dy as i32 / 4) as u8;
    hsv.v = qadd8(hsv.v, (255 - effect as i32) as u8);
    hsv
}

fn splash(_: &Engine, mut hsv: Hsv, _: i16, _: i16, dist: u8, tick: u16) -> Hsv {
    let mut effect = (tick as i32 - dist as i32) as u16;
    if effect > 255 {
        effect = 255;
    }
    hsv.h = (hsv.h as i32 + effect as i32) as u8;
    hsv.v = qadd8(hsv.v, (255 - effect as i32) as u8);
    hsv
}

fn solid_splash(_: &Engine, mut hsv: Hsv, _: i16, _: i16, dist: u8, tick: u16) -> Hsv {
    let mut effect = (tick as i32 - dist as i32) as u16;
    if effect > 255 {
        effect = 255;
    }
    hsv.v = qadd8(hsv.v, (255 - effect as i32) as u8);
    hsv
}
