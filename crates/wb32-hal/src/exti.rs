//! Async edge/level waits on GPIO pins through EXTI lines 0-15.
//!
//! Line n can watch pin n of one port at a time (AFIO EXTICR selects the
//! port), so two `ExtiInput`s on the same pin number panic.
//!
//! As in embassy-stm32: a wait unmasks its line (IMR); the interrupt handler
//! masks every line that fired and wakes its task, and the future completes
//! once it sees its line masked. Register setup follows ChibiOS's
//! `_pal_lld_enablepadevent`.

use core::future::Future;
use core::pin::Pin as FuturePin;
use core::sync::atomic::{AtomicU16, Ordering};
use core::task::{Context, Poll};

use embassy_sync::waitqueue::AtomicWaker;
use wb32_pac::{AFIO, EXTI, Interrupt, interrupt};

use crate::gpio::{AnyPin, Input, Level, Port, Pull};
use crate::rcc;

const LINES: usize = 16;

static WAKERS: [AtomicWaker; LINES] = [const { AtomicWaker::new() }; LINES];
/// Lines owned by an `ExtiInput`.
static USED: AtomicU16 = AtomicU16::new(0);

fn irq_for_line(line: u8) -> Interrupt {
    match line {
        0 => Interrupt::EXTI0,
        1 => Interrupt::EXTI1,
        2 => Interrupt::EXTI2,
        3 => Interrupt::EXTI3,
        4 => Interrupt::EXTI4,
        5..=9 => Interrupt::EXTI9_5,
        _ => Interrupt::EXTI15_10,
    }
}

/// A GPIO input that can also wait for edges and levels.
pub struct ExtiInput {
    pin: Input,
}

impl ExtiInput {
    /// `prio` is the NVIC priority of this line's interrupt (shared by lines
    /// 5-9 and 10-15).
    pub fn new(pin: impl Into<AnyPin>, pull: Pull, prio: u8) -> Self {
        let pin = Input::new(pin, pull);
        let line = pin.pin().pin();
        // Port D has only pins 0-2 on EXTI (RM2905025 §10.4, Table 10-1).
        assert!(
            pin.pin().port() != Port::D || line <= 2,
            "no EXTI line for this port D pin"
        );
        let mask = 1u16 << line;
        let prev = USED.fetch_or(mask, Ordering::AcqRel);
        assert!(prev & mask == 0, "EXTI line already in use");

        rcc::enable_apb1(|w| {
            w.set_extien(true);
            w.set_afioen(true);
        });
        critical_section::with(|_| {
            AFIO.exticr(line as usize / 4)
                .modify(|w| w.set_exti(line as usize % 4, pin.pin().port() as u8));
        });

        let irq = irq_for_line(line);
        crate::unmask_irq(irq, prio);
        ExtiInput { pin }
    }

    pub fn is_high(&self) -> bool {
        self.pin.is_high()
    }
    pub fn is_low(&self) -> bool {
        self.pin.is_low()
    }
    pub fn level(&self) -> Level {
        self.pin.level()
    }

    fn line(&self) -> u8 {
        self.pin.pin().pin()
    }

    pub async fn wait_for_high(&mut self) {
        // Arm before checking the level, so an edge in between isn't lost.
        let fut = ExtiFuture::new(self.line(), true, false);
        if self.is_high() {
            return;
        }
        fut.await
    }

    pub async fn wait_for_low(&mut self) {
        let fut = ExtiFuture::new(self.line(), false, true);
        if self.is_low() {
            return;
        }
        fut.await
    }

    pub async fn wait_for_rising_edge(&mut self) {
        ExtiFuture::new(self.line(), true, false).await
    }

    pub async fn wait_for_falling_edge(&mut self) {
        ExtiFuture::new(self.line(), false, true).await
    }

    pub async fn wait_for_any_edge(&mut self) {
        ExtiFuture::new(self.line(), true, true).await
    }
}

impl Drop for ExtiInput {
    fn drop(&mut self) {
        USED.fetch_and(!(1u16 << self.line()), Ordering::AcqRel);
    }
}

struct ExtiFuture {
    line: u8,
}

impl ExtiFuture {
    fn new(line: u8, rising: bool, falling: bool) -> Self {
        let mask = 1u32 << line;
        critical_section::with(|_| {
            EXTI.rtsr().modify(|w| w.set_line(line as usize, rising));
            EXTI.ftsr().modify(|w| w.set_line(line as usize, falling));
            // PR clears on writing 1.
            EXTI.pr().write_value(wb32_pac::exti::regs::Pr(mask));
            EXTI.imr().modify(|w| w.set_line(line as usize, true));
        });
        ExtiFuture { line }
    }
}

impl Future for ExtiFuture {
    type Output = ();

    fn poll(self: FuturePin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        WAKERS[self.line as usize].register(cx.waker());
        if EXTI.imr().read().line(self.line as usize) {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl Drop for ExtiFuture {
    fn drop(&mut self) {
        let line = self.line as usize;
        critical_section::with(|_| {
            EXTI.imr().modify(|w| w.set_line(line, false));
            EXTI.rtsr().modify(|w| w.set_line(line, false));
            EXTI.ftsr().modify(|w| w.set_line(line, false));
            EXTI.pr().write_value(wb32_pac::exti::regs::Pr(1 << line));
        });
    }
}

/// Masks and wakes every line in `lines` that fired.
fn on_irq(lines: u32) {
    let fired = EXTI.pr().read().0 & EXTI.imr().read().0 & lines;
    if fired == 0 {
        return;
    }
    critical_section::with(|_| {
        EXTI.imr().modify(|w| w.0 &= !fired);
    });
    EXTI.pr().write_value(wb32_pac::exti::regs::Pr(fired));
    for (line, waker) in WAKERS.iter().enumerate() {
        if fired & (1 << line) != 0 {
            waker.wake();
        }
    }
}

#[interrupt]
fn EXTI0() {
    on_irq(1 << 0);
}
#[interrupt]
fn EXTI1() {
    on_irq(1 << 1);
}
#[interrupt]
fn EXTI2() {
    on_irq(1 << 2);
}
#[interrupt]
fn EXTI3() {
    on_irq(1 << 3);
}
#[interrupt]
fn EXTI4() {
    on_irq(1 << 4);
}
#[interrupt]
fn EXTI9_5() {
    on_irq(0x03E0);
}
#[interrupt]
fn EXTI15_10() {
    on_irq(0xFC00);
}

impl embedded_hal::digital::ErrorType for ExtiInput {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::InputPin for ExtiInput {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(ExtiInput::is_high(self))
    }
    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(ExtiInput::is_low(self))
    }
}

impl embedded_hal_async::digital::Wait for ExtiInput {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        ExtiInput::wait_for_high(self).await;
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        ExtiInput::wait_for_low(self).await;
        Ok(())
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        ExtiInput::wait_for_rising_edge(self).await;
        Ok(())
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        ExtiInput::wait_for_falling_edge(self).await;
        Ok(())
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        ExtiInput::wait_for_any_edge(self).await;
        Ok(())
    }
}
