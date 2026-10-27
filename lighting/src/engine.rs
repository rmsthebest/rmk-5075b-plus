//! rgb_matrix.c: the task state machine, the hit tracker and the LED buffer.
//!
//! `Engine::task` is one `rgb_matrix_task()` call. QMK runs it on every pass
//! of its main loop: STARTING snapshots the timer and the hit tracker,
//! RENDERING runs the effect over `PROCESS_LIMIT` LEDs a call until it says
//! it's done, FLUSHING sends the LEDs if any changed, and SYNCING waits until
//! `FLUSH_LIMIT` ms after the snapshot.

use crate::color::{Hsv, Rgb};
use crate::effects::{self, effect};
use crate::layout::{LED_COUNT, MATRIX_CO, MATRIX_COLS, MATRIX_ROWS, NO_LED, POINTS};
use crate::math::{Rand, qadd8, qsub8};

/// RGB_MATRIX_LED_PROCESS_LIMIT: LEDs rendered per task call.
pub const PROCESS_LIMIT: u8 = ((LED_COUNT + 4) / 5) as u8;
/// RGB_MATRIX_LED_FLUSH_LIMIT: ms from one frame's start to the next.
pub const FLUSH_LIMIT: u32 = 16;
/// The board's steps (keyboard.json val_steps / speed_steps; QMK's
/// defaults for hue and saturation) and RGB_MATRIX_MAXIMUM_BRIGHTNESS.
pub const HUE_STEP: u8 = 8;
pub const SAT_STEP: u8 = 16;
pub const VAL_STEP: u8 = 21;
pub const SPD_STEP: u8 = 51;
pub const MAX_BRIGHTNESS: u8 = 84;
/// LED_HITS_TO_REMEMBER.
pub const HITS: usize = 8;
pub const LED_FLAG_ALL: u8 = 0xFF;
pub const LED_FLAG_MODIFIER: u8 = 0x01;

/// rgb_config_t, the part effects read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub enable: bool,
    pub mode: u8,
    pub hsv: Hsv,
    pub speed: u8,
    pub flags: u8,
}

impl Default for Config {
    /// The board's defaults (keyboard.json: cycle_left_right, speed 153,
    /// val 84 = RGB_MATRIX_MAXIMUM_BRIGHTNESS).
    fn default() -> Self {
        Config {
            enable: true,
            mode: effect::CYCLE_LEFT_RIGHT,
            hsv: Hsv {
                h: 0,
                s: 255,
                v: 84,
            },
            speed: 153,
            flags: LED_FLAG_ALL,
        }
    }
}

/// The LED buffer (rgb_matrix_ws2812_array) and its dirty flag.
pub struct Leds {
    pub rgb: [Rgb; LED_COUNT],
    pub dirty: bool,
}

impl Leds {
    pub fn set(&mut self, i: usize, c: Rgb) {
        if i >= LED_COUNT || self.rgb[i] == c {
            return;
        }
        self.dirty = true;
        self.rgb[i] = c;
    }

    pub fn set_all(&mut self, c: Rgb) {
        for i in 0..LED_COUNT {
            self.set(i, c);
        }
    }
}

/// The board's hooks into the task.
pub trait Hooks {
    /// rgb_matrix_indicators_kb: once a frame, after the last chunk renders.
    fn indicators(&mut self, _leds: &mut Leds) {}
    /// rgb_matrix_indicators_advanced_kb: after every chunk, with its LEDs.
    /// The board may change the config here (EE_CLR's reset does).
    fn indicators_advanced(&mut self, _config: &mut Config, _leds: &mut Leds, _min: u8, _max: u8) {}
    /// The board's own effect (`effect::RGBR_PLAY`) for LEDs min..max.
    fn custom_effect(&mut self, _config: &Config, _leds: &mut Leds, _min: u8, _max: u8) {}
}

pub struct NoHooks;
impl Hooks for NoHooks {}

/// last_hit_t.
#[derive(Clone, Copy)]
pub struct HitTracker {
    pub count: u8,
    pub x: [u8; HITS],
    pub y: [u8; HITS],
    pub index: [u8; HITS],
    pub tick: [u16; HITS],
}

impl HitTracker {
    const fn new() -> Self {
        HitTracker {
            count: 0,
            x: [0; HITS],
            y: [0; HITS],
            index: [0; HITS],
            tick: [u16::MAX; HITS],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaskState {
    Starting,
    Rendering,
    Flushing,
    Syncing,
}

/// effect_params_t.
#[derive(Clone, Copy)]
pub(crate) struct Params {
    pub iter: u8,
    pub flags: u8,
    pub init: bool,
}

/// The effects' `static` variables, which live across effect changes as in C.
pub(crate) struct Statics {
    pub pixel_rain_wait: u32,
    pub pixel_flow_led: [Rgb; LED_COUNT],
    pub pixel_flow_wait: u32,
    pub fractal_led: [[bool; effects::MID_COL]; MATRIX_ROWS],
    pub fractal_wait: u32,
    pub heatmap_decrease_timer: u16,
    pub heatmap_decrease: bool,
    pub rain_drop: u8,
    pub rain_decay: u8,
}

pub struct Engine {
    pub config: Config,
    pub leds: Leds,
    /// suspend_state: renders effect 0 (off) while set.
    pub suspended: bool,
    pub(crate) timer: u32,
    timer_buffer: u32,
    /// The current time, as timer_read32() would give it inside `task`.
    pub(crate) now: u32,
    pub(crate) hits: HitTracker,
    hit_buffer: HitTracker,
    state: TaskState,
    pub(crate) params: Params,
    last_effect: u8,
    last_enable: u8,
    pub(crate) rand: Rand,
    /// newlib's rand() state.
    pub(crate) crand: u64,
    pub(crate) frame_buffer: [[u8; MATRIX_COLS]; MATRIX_ROWS],
    pub(crate) st: Statics,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Engine {
            config,
            leds: Leds {
                rgb: [Rgb::OFF; LED_COUNT],
                dirty: false,
            },
            suspended: false,
            timer: 0,
            timer_buffer: 0,
            now: 0,
            hits: HitTracker::new(),
            hit_buffer: HitTracker::new(),
            state: TaskState::Syncing,
            params: Params {
                iter: 0,
                flags: LED_FLAG_ALL,
                init: false,
            },
            last_effect: u8::MAX,
            last_enable: u8::MAX,
            rand: Rand::default(),
            crand: 1,
            frame_buffer: [[0; MATRIX_COLS]; MATRIX_ROWS],
            st: Statics {
                pixel_rain_wait: 0,
                pixel_flow_led: [Rgb::OFF; LED_COUNT],
                pixel_flow_wait: 0,
                fractal_led: [[false; effects::MID_COL]; MATRIX_ROWS],
                fractal_wait: 0,
                heatmap_decrease_timer: 0,
                heatmap_decrease: false,
                rain_drop: 0,
                rain_decay: 0,
            },
        }
    }

    /// One rgb_matrix_task() at `now` ms. True when it flushed changed LEDs
    /// (`leds.rgb` is then the frame to send).
    pub fn task(&mut self, now: u32, hooks: &mut impl Hooks) -> bool {
        self.now = now;
        self.task_timers();
        let effect = if self.suspended || !self.config.enable {
            effect::NONE
        } else {
            self.config.mode
        };
        match self.state {
            TaskState::Starting => self.task_start(),
            TaskState::Rendering => {
                self.task_render(effect, hooks);
                if effect != effect::NONE {
                    if self.state == TaskState::Flushing {
                        hooks.indicators(&mut self.leds);
                    }
                    let (min, max) = limits(self.params.iter.wrapping_sub(1));
                    hooks.indicators_advanced(&mut self.config, &mut self.leds, min, max);
                }
            }
            TaskState::Flushing => return self.task_flush(effect),
            TaskState::Syncing => {
                if now.wrapping_sub(self.timer) >= FLUSH_LIMIT {
                    self.state = TaskState::Starting;
                }
            }
        }
        false
    }

    /// rgb_matrix_handle_key_event (RGB_MATRIX_KEYPRESSES).
    pub fn key_event(&mut self, row: u8, col: u8, pressed: bool) {
        let mut led = [0u8; HITS];
        let mut led_count = 0usize;
        if pressed {
            let i = MATRIX_CO[row as usize][col as usize];
            if i != NO_LED {
                led[0] = i;
                led_count = 1;
            }
        }
        let b = &mut self.hit_buffer;
        if b.count as usize + led_count > HITS {
            let n = HITS - led_count;
            b.x.copy_within(led_count..led_count + n, 0);
            b.y.copy_within(led_count..led_count + n, 0);
            b.tick.copy_within(led_count..led_count + n, 0);
            b.index.copy_within(led_count..led_count + n, 0);
            b.count = n as u8;
        }
        for &l in &led[..led_count] {
            let k = b.count as usize;
            b.x[k] = POINTS[l as usize].0;
            b.y[k] = POINTS[l as usize].1;
            b.index[k] = l;
            b.tick[k] = 0;
            b.count += 1;
        }
        if pressed && self.config.mode == effect::TYPING_HEATMAP {
            self.heatmap_key(row, col);
        }
    }

    /// rgb_matrix_set_suspend_state (RGB_MATRIX_SLEEP): turning off renders
    /// and flushes effect 0 at once; true when that changed the LEDs.
    pub fn set_suspended(&mut self, state: bool, hooks: &mut impl Hooks) -> bool {
        let mut flushed = false;
        if state && !self.suspended {
            self.task_render(effect::NONE, hooks);
            flushed = self.task_flush(effect::NONE);
        }
        self.suspended = state;
        flushed
    }

    // ---- rgb_matrix.c's setters (the *_noeeprom ones; saving is the caller's) ----

    /// True when the next `task` call starts a new frame: a frame is due,
    /// or a setter restarted the task (QMK sets STARTING).
    pub fn frame_due(&self, now: u32) -> bool {
        match self.state {
            TaskState::Syncing => now.wrapping_sub(self.timer) >= FLUSH_LIMIT,
            _ => true,
        }
    }

    /// When the current frame's 16 ms are up.
    pub fn next_frame(&self) -> u32 {
        self.timer.wrapping_add(FLUSH_LIMIT)
    }

    pub fn toggle(&mut self) {
        self.config.enable = !self.config.enable;
        self.state = TaskState::Starting;
    }

    pub fn enable(&mut self) {
        if !self.config.enable {
            self.state = TaskState::Starting;
        }
        self.config.enable = true;
    }

    pub fn disable(&mut self) {
        if self.config.enable {
            self.state = TaskState::Starting;
        }
        self.config.enable = false;
    }

    pub fn set_mode(&mut self, mode: u8) {
        if !self.config.enable {
            return;
        }
        self.config.mode = mode.clamp(1, effect::COUNT - 1);
        self.state = TaskState::Starting;
    }

    pub fn step(&mut self) {
        let mode = self.config.mode.wrapping_add(1);
        self.set_mode(if mode < effect::COUNT { mode } else { 1 });
    }

    pub fn step_reverse(&mut self) {
        let mode = self.config.mode.wrapping_sub(1);
        self.set_mode(if mode < 1 { effect::COUNT - 1 } else { mode });
    }

    pub fn sethsv(&mut self, h: u8, s: u8, v: u8) {
        if !self.config.enable {
            return;
        }
        self.config.hsv = Hsv {
            h,
            s,
            v: v.min(MAX_BRIGHTNESS),
        };
    }

    pub fn increase_hue(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h.wrapping_add(HUE_STEP), c.s, c.v);
    }

    pub fn decrease_hue(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h.wrapping_sub(HUE_STEP), c.s, c.v);
    }

    pub fn increase_sat(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h, qadd8(c.s, SAT_STEP), c.v);
    }

    pub fn decrease_sat(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h, qsub8(c.s, SAT_STEP), c.v);
    }

    pub fn increase_val(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h, c.s, qadd8(c.v, VAL_STEP));
    }

    pub fn decrease_val(&mut self) {
        let c = self.config.hsv;
        self.sethsv(c.h, c.s, qsub8(c.v, VAL_STEP));
    }

    pub fn set_speed(&mut self, speed: u8) {
        self.config.speed = speed;
    }

    pub fn increase_speed(&mut self) {
        self.config.speed = qadd8(self.config.speed, SPD_STEP);
    }

    pub fn decrease_speed(&mut self) {
        self.config.speed = qsub8(self.config.speed, SPD_STEP);
    }

    pub fn set_flags(&mut self, flags: u8) {
        self.config.flags = flags;
    }

    fn task_timers(&mut self) {
        let delta = self.now.wrapping_sub(self.timer_buffer);
        self.timer_buffer = self.now;
        let b = &mut self.hit_buffer;
        let count = b.count as usize;
        for i in 0..count {
            if (u16::MAX as u32).wrapping_sub(delta) < b.tick[i] as u32 {
                b.count -= 1;
                continue;
            }
            b.tick[i] = b.tick[i].wrapping_add(delta as u16);
        }
    }

    fn task_start(&mut self) {
        self.params.iter = 0;
        self.timer = self.timer_buffer;
        self.hits = self.hit_buffer;
        self.state = TaskState::Rendering;
    }

    fn task_render(&mut self, effect: u8, hooks: &mut impl Hooks) {
        self.params.init =
            effect != self.last_effect || self.config.enable as u8 != self.last_enable;
        if self.params.flags != self.config.flags {
            self.params.flags = self.config.flags;
            self.leds.set_all(Rgb::OFF);
        }
        let rendering = self.render(effect, hooks);
        self.params.iter = self.params.iter.wrapping_add(1);
        if !rendering {
            self.state = TaskState::Flushing;
            if !self.params.init && effect == effect::NONE {
                self.state = TaskState::Syncing;
            }
        }
    }

    fn task_flush(&mut self, effect: u8) -> bool {
        self.last_effect = effect;
        self.last_enable = self.config.enable as u8;
        self.state = TaskState::Syncing;
        core::mem::replace(&mut self.leds.dirty, false)
    }

    /// The flag test effects make before touching LED `i`.
    pub(crate) fn has_flags(&self, i: usize) -> bool {
        crate::layout::FLAGS.get(i).copied().unwrap_or(0) & self.params.flags != 0
    }

    pub(crate) fn speed_plus(&self, n: u8) -> u8 {
        qadd8(self.config.speed, n)
    }
}

/// rgb_matrix_get_limits: the LEDs chunk `iter` renders.
pub fn limits(iter: u8) -> (u8, u8) {
    let min = (PROCESS_LIMIT as u16 * iter as u16) as u8;
    let max = min.wrapping_add(PROCESS_LIMIT).min(LED_COUNT as u8);
    (min, max)
}

/// rgb_matrix_check_finished_leds: true while chunks remain.
pub(crate) fn more_after(led_max: u8) -> bool {
    (led_max as usize) < LED_COUNT
}
