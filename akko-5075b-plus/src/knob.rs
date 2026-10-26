//! The knob: a quadrature encoder on B7 (A) and B6 (B), pulled up, read on
//! every edge through EXTI lines 7 and 6. The decoding is QMK's
//! (`drivers/encoder/encoder_quadrature.c`, resolution 4), so a detent turns
//! the same way as on the QMK build: +4 is counter-clockwise, -4 clockwise.
//!
//! RMK's own `RotaryEncoder` only waits on edges with the `async_matrix`
//! feature, and otherwise polls every 20 ms, too slow for a quick turn.

use embassy_futures::select::select;
use embassy_time::Timer;
use rmk::event::KeyboardEvent;
use rmk::macros::input_device;
use rmk::input_device::rotary_encoder::Direction;
use wb32_hal::exti::ExtiInput;

const LUT: [i8; 16] = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];
const RESOLUTION: i8 = 4;
/// How long a step's "key" is held, as QMK's ENCODER_MAP_KEY_DELAY (1 ms in
/// config.h) plus margin for the host's 1 ms poll.
const TAP_MS: u64 = 5;

#[input_device(publish = KeyboardEvent)]
pub struct Knob {
    a: ExtiInput,
    b: ExtiInput,
    id: u8,
    /// The last two readings, 2 bits each (QMK's encoder_state).
    state: u8,
    pulses: i8,
    /// A step whose release is still to be sent.
    held: Option<Direction>,
}

impl Knob {
    pub fn new(a: ExtiInput, b: ExtiInput, id: u8) -> Self {
        let state = (a.is_high() as u8) | ((b.is_high() as u8) << 1);
        Knob { a, b, id, state, pulses: 0, held: None }
    }

    async fn read_keyboard_event(&mut self) -> KeyboardEvent {
        if let Some(dir) = self.held.take() {
            Timer::after_millis(TAP_MS).await;
            return KeyboardEvent::rotary_encoder(self.id, dir, false);
        }
        loop {
            let now = (self.a.is_high() as u8) | ((self.b.is_high() as u8) << 1);
            if now != self.state & 3 {
                self.state = (self.state << 2) | now;
                self.pulses += LUT[(self.state & 0xF) as usize];
                let dir = if self.pulses >= RESOLUTION {
                    Some(Direction::CounterClockwise)
                } else if self.pulses <= -RESOLUTION {
                    Some(Direction::Clockwise)
                } else {
                    None
                };
                self.pulses %= RESOLUTION;
                if let Some(dir) = dir {
                    self.held = Some(dir);
                    return KeyboardEvent::rotary_encoder(self.id, dir, true);
                }
                continue;
            }
            // An edge between the read above and arming EXTI would only be
            // seen at the next one; the timeout catches it.
            select(
                select(self.a.wait_for_any_edge(), self.b.wait_for_any_edge()),
                Timer::after_millis(5),
            )
            .await;
        }
    }
}
