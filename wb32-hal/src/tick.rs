//! A periodic interrupt on TIM2, for checks that must run even when the
//! executor doesn't (the app's software watchdog).
//!
//! Only TIM2's update interrupt (the counter's wrap) is used: on the board
//! it fires as it should, unlike its compare channels (see
//! src/time_driver.rs, which moved to SysTick because of them). TIM2 is
//! also the one timer the simulator models.

use core::sync::atomic::{AtomicUsize, Ordering};

use wb32_pac::{Interrupt, TIM2, interrupt, tim};

use crate::rcc;

/// TIM2 counts at 10 kHz.
const PSC: u32 = rcc::PCLK1_HZ / 10_000 - 1;
const _: () = assert!(PSC <= 0xFFFF);

/// The callback, a `fn()` as usize (0: none).
static CALLBACK: AtomicUsize = AtomicUsize::new(0);

/// Calls `f` from TIM2's interrupt every `period_ms` (1 to 6553), at
/// interrupt priority `prio` (upper 4 bits used). Call after [`rcc::init`].
pub fn start(period_ms: u32, prio: u8, f: fn()) {
    assert!((1..=6553).contains(&period_ms));
    CALLBACK.store(f as usize, Ordering::Relaxed);
    rcc::enable_apb1(|w| w.set_tim2en(true));
    let r = TIM2;
    r.cr1().write(|w| w.set_urs(true));
    r.psc().write_value(tim::regs::Psc(PSC));
    r.arr().write_value(tim::regs::Arr(period_ms * 10 - 1));
    // Load PSC now rather than at the first wrap.
    r.egr().write(|w| w.set_ug(true));
    r.cnt().write_value(tim::regs::Cnt(0));
    r.sr().write_value(tim::regs::Sr(0));
    r.dier().write(|w| w.set_uie(true));
    unsafe {
        let mut nvic = cortex_m::Peripherals::steal().NVIC;
        nvic.set_priority(Interrupt::TIM2, prio);
        cortex_m::peripheral::NVIC::unpend(Interrupt::TIM2);
        cortex_m::peripheral::NVIC::unmask(Interrupt::TIM2);
    }
    r.cr1().modify(|w| w.set_cen(true));
}

#[interrupt]
fn TIM2() {
    // SR flags clear on writing 0.
    TIM2.sr().write_value(tim::regs::Sr(0));
    let f = CALLBACK.load(Ordering::Relaxed);
    if f != 0 {
        // SAFETY: only `start` stores it, from a `fn()`.
        let f: fn() = unsafe { core::mem::transmute(f) };
        f();
    }
}
