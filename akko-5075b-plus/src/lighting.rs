//! RGB lighting (M5): QMK's rgb_matrix (the `lighting` crate) on the 106
//! WS2812 LEDs (wb32_hal::ws2812, SPIM2 on B15).
//!
//! The task runs a frame every 16 ms (RGB_MATRIX_LED_FLUSH_LIMIT), all
//! chunks of it back to back (QMK spreads them over main-loop passes a
//! fraction of a millisecond apart), and sends it only when an LED changed.
//! Key presses go to the hit tracker for the reactive effects, except the
//! two positions the Win/Mac switch taps (keymap::SWITCH_*): on QMK the
//! switch is a GPIO, not a key, and (5, 3) is under LED 105, a side light.
//!
//! Power: A8 enables the LEDs' supply, on while lighting is on with a
//! brightness above 0 (5075b_plus.c's keyboard_post_init_kb); A9, the boost
//! converter, stays on.
//!
//! The RGB keys act as QMK's process_rgb_matrix (without Shift reversing
//! them). Still to come: the board's own key behaviour and side lights
//! (5075b_plus.c, rgb_record/), and saving the settings.

use embassy_time::Instant;
use lighting_crate::{Engine, NoHooks};
use rmk::event::{ActionEvent, KeyboardEvent, KeyboardEventPos};
use rmk::macros::processor;
use rmk::processor::DeadlineProcessor;
use rmk::types::action::{Action, LightAction};
use wb32_hal::gpio::Output;
use wb32_hal::ws2812::Ws2812;

use crate::keymap::{SWITCH_MAC, SWITCH_WIN};

#[processor(subscribe = [KeyboardEvent, ActionEvent])]
#[::rmk::macros::runnable_generated]
pub struct Lighting {
    engine: &'static mut Engine,
    leds: Ws2812,
    power: Output,
}

impl Lighting {
    /// `engine` from `Engine::new(Config::default())`, kept outside the
    /// main task (src/main.rs `ram2`).
    pub fn new(engine: &'static mut Engine, leds: Ws2812, power: Output) -> Self {
        let mut l = Lighting { engine, leds, power };
        l.update_power();
        l
    }

    fn update_power(&mut self) {
        let c = &self.engine.config;
        if c.enable && c.hsv.v > 0 {
            self.power.set_high();
        } else {
            self.power.set_low();
        }
    }

    async fn on_keyboard_event(&mut self, e: KeyboardEvent) {
        if let KeyboardEventPos::Key(k) = e.pos {
            if (k.row, k.col) != SWITCH_WIN && (k.row, k.col) != SWITCH_MAC {
                self.engine.key_event(k.row, k.col, e.pressed);
            }
        }
    }

    async fn on_action_event(&mut self, e: ActionEvent) {
        let Action::Light(a) = e.action else { return };
        // RGB_TRIGGER_ON_KEYDOWN.
        if !e.keyboard_event.pressed {
            return;
        }
        let en = &mut self.engine;
        match a {
            LightAction::RgbTog => en.toggle(),
            LightAction::RgbModeForward => en.step(),
            LightAction::RgbModeReverse => en.step_reverse(),
            LightAction::RgbHui => en.increase_hue(),
            LightAction::RgbHud => en.decrease_hue(),
            LightAction::RgbSai => en.increase_sat(),
            LightAction::RgbSad => en.decrease_sat(),
            LightAction::RgbVai => en.increase_val(),
            LightAction::RgbVad => en.decrease_val(),
            LightAction::RgbSpi => en.increase_speed(),
            LightAction::RgbSpd => en.decrease_speed(),
            _ => return,
        }
        self.update_power();
    }

    /// Runs one whole frame: rgb_matrix_task() until the frame is sent (or
    /// found unchanged).
    async fn frame(&mut self) {
        let now = now_ms();
        loop {
            if self.engine.task(now, &mut NoHooks) {
                self.leds.write(self.engine.leds.rgb.iter().map(|c| [c.r, c.g, c.b])).await;
            }
            if !self.engine.frame_due(now) {
                break;
            }
        }
    }
}

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

impl DeadlineProcessor for Lighting {
    fn deadline(&self) -> Option<Instant> {
        let now = now_ms();
        if self.engine.frame_due(now) {
            return Some(Instant::now());
        }
        let wait = self.engine.next_frame().wrapping_sub(now);
        Some(Instant::now() + embassy_time::Duration::from_millis(wait as u64))
    }

    async fn on_deadline(&mut self) {
        self.frame().await;
    }
}

impl rmk::core_traits::Runnable for Lighting {
    async fn run(&mut self) -> ! {
        self.deadline_loop().await
    }
}
