//! The knob: a quadrature encoder on B7 (A) and B6 (B), pulled up, read on
//! every edge through EXTI lines 7 and 6. The decoding is QMK's
//! (`drivers/encoder/encoder_quadrature.c`, resolution 4), so a detent turns
//! the same way as on the QMK build: +4 is counter-clockwise, -4 clockwise.
//!
//! Between steps it waits for either pin to change level, with no timer:
//! each wait arms its EXTI line before it reads the pin, so a change in
//! between isn't lost. Asleep (src/power.rs), a change wakes the keyboard
//! and the turn is sent. A step counts as input for the sleep timeout.
//!
//! RMK's own `RotaryEncoder` only waits on edges with the `async_matrix`
//! feature, and otherwise polls every 20 ms, too slow for a quick turn.

use embassy_futures::select::select;
use embassy_time::{Duration, Instant, Timer};
use rmk::event::KeyboardEvent;
use rmk::input_device::rotary_encoder::Direction;
use rmk::macros::input_device;
use wb32_hal::exti::ExtiInput;

const LUT: [i8; 16] = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];
const RESOLUTION: i8 = 4;
/// How long a step's "key" is held, as QMK's ENCODER_MAP_KEY_DELAY (1 ms in
/// config.h) plus margin for the host's 1 ms poll.
const TAP: Duration = Duration::from_millis(5);
/// Steps turned faster than they can be sent wait here, up to this many.
const MAX_QUEUED: i8 = 8;

#[input_device(publish = KeyboardEvent)]
pub struct Knob {
    a: ExtiInput,
    b: ExtiInput,
    id: u8,
    /// The last two readings, 2 bits each (QMK's encoder_state).
    state: u8,
    pulses: i8,
    /// Steps decoded but not sent yet: > 0 counter-clockwise, < 0 clockwise.
    queued: i8,
    /// The step being held, and when to release it.
    held: Option<(Direction, Instant)>,
}

impl Knob {
    pub fn new(a: ExtiInput, b: ExtiInput, id: u8) -> Self {
        let state = (a.is_high() as u8) | ((b.is_high() as u8) << 1);
        Knob { a, b, id, state, pulses: 0, queued: 0, held: None }
    }

    /// Follows the pins since the last call, queueing whole steps.
    fn decode(&mut self) {
        let now = (self.a.is_high() as u8) | ((self.b.is_high() as u8) << 1);
        if now == self.state & 3 {
            return;
        }
        self.state = (self.state << 2) | now;
        self.pulses += LUT[(self.state & 0xF) as usize];
        if self.pulses >= RESOLUTION {
            self.queued = (self.queued + 1).min(MAX_QUEUED);
        } else if self.pulses <= -RESOLUTION {
            self.queued = (self.queued - 1).max(-MAX_QUEUED);
        }
        self.pulses %= RESOLUTION;
    }

    async fn read_keyboard_event(&mut self) -> KeyboardEvent {
        loop {
            self.decode();
            let now = Instant::now();
            match self.held {
                Some((dir, until)) if now >= until => {
                    self.held = None;
                    return KeyboardEvent::rotary_encoder(self.id, dir, false);
                }
                None if self.queued != 0 => {
                    let dir = if self.queued > 0 { Direction::CounterClockwise } else { Direction::Clockwise };
                    self.queued -= self.queued.signum();
                    self.held = Some((dir, now + TAP));
                    crate::power::input();
                    return KeyboardEvent::rotary_encoder(self.id, dir, true);
                }
                _ => {}
            }
            // Either pin leaving the level last decoded.
            let (a_high, b_high) = (self.state & 1 != 0, self.state & 2 != 0);
            let (a, b) = (&mut self.a, &mut self.b);
            let moved = select(
                async move { if a_high { a.wait_for_low().await } else { a.wait_for_high().await } },
                async move { if b_high { b.wait_for_low().await } else { b.wait_for_high().await } },
            );
            match self.held {
                Some((_, until)) => {
                    select(moved, Timer::at(until)).await;
                }
                None => {
                    moved.await;
                    if crate::power::sleeping() {
                        crate::power::input();
                    }
                }
            }
        }
    }
}
