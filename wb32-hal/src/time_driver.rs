//! embassy-time driver on TIM2 (32-bit), 1 MHz tick.
//!
//! Same scheme as embassy-stm32's timer driver, widened to a 32-bit counter:
//! - The counter runs freely over the full 32-bit range.
//! - `period` counts half-wraps: the update event (wrap to 0) and CC1 at
//!   0x8000_0000 each add one, so `now = period << 31 + (CNT with its top bit
//!   corrected by period's parity)`. This stays right even if a half-wrap
//!   interrupt is pending while `now()` runs.
//! - CC2 is the alarm. It's only armed once the alarm is less than one
//!   half-period plus margin away (`next_period` arms it then), so a CCR2
//!   match can never come a full wrap early.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering, compiler_fence};
use core::task::Waker;

use critical_section::{CriticalSection, Mutex};
use embassy_time_driver::{Driver, TICK_HZ};
use embassy_time_queue_utils::Queue;
use wb32_pac::{Interrupt, TIM2, interrupt, tim};

use crate::rcc;

const HALF: u32 = 1 << 31;

// TIM2 runs from PCLK1; the prescaler brings it down to the tick rate.
const _: () = assert!((rcc::PCLK1_HZ as u64).is_multiple_of(TICK_HZ));
const PSC: u32 = (rcc::PCLK1_HZ as u64 / TICK_HZ) as u32 - 1;

/// Counter value at start-up. The simulator test builds start 2 s before
/// the counter's half-wrap or full wrap (as if the chip had been up for 36 or
/// 71 minutes), so a short run crosses that boundary.
#[cfg(not(any(feature = "sim-time-near-half", feature = "sim-time-near-wrap")))]
const START: u32 = 0;
#[cfg(feature = "sim-time-near-half")]
const START: u32 = HALF - 2 * TICK_HZ as u32;
#[cfg(feature = "sim-time-near-wrap")]
const START: u32 = 0u32.wrapping_sub(2 * TICK_HZ as u32);

fn calc_now(period: u32, counter: u32) -> u64 {
    ((period as u64) << 31) + (counter ^ ((period & 1) << 31)) as u64
}

// repr(C) keeps `period` at offset 0: the simulator tests read it.
#[repr(C)]
struct TimDriver {
    /// Number of half-wraps of the 32-bit counter.
    period: AtomicU32,
    /// Next alarm time, `u64::MAX` when none.
    alarm: Mutex<Cell<u64>>,
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: TimDriver = TimDriver {
    period: AtomicU32::new(START >> 31),
    alarm: Mutex::new(Cell::new(u64::MAX)),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

/// Starts TIM2 as the embassy-time base. Call after [`rcc::init`].
/// `prio` is the TIM2 interrupt priority (upper 4 bits used).
pub fn init(prio: u8) {
    rcc::enable_apb1(|w| w.set_tim2en(true));

    let r = TIM2;
    r.cr1().write(|w| {
        // Only counter overflow sets UIF, not our own UG below.
        w.set_urs(true);
    });
    r.psc().write_value(tim::regs::Psc(PSC));
    r.arr().write_value(tim::regs::Arr(u32::MAX));
    // Load PSC now rather than at the first wrap.
    r.egr().write(|w| w.set_ug(true));
    r.cnt().write_value(tim::regs::Cnt(START));
    r.ccr1().write_value(tim::regs::Ccr1(HALF));
    r.sr().write_value(tim::regs::Sr(0));
    r.dier().write(|w| {
        w.set_uie(true);
        w.set_cc1ie(true);
    });

    unsafe {
        let mut nvic = cortex_m::Peripherals::steal().NVIC;
        nvic.set_priority(Interrupt::TIM2, prio);
        cortex_m::peripheral::NVIC::unpend(Interrupt::TIM2);
        cortex_m::peripheral::NVIC::unmask(Interrupt::TIM2);
    }

    r.cr1().modify(|w| w.set_cen(true));
}

impl TimDriver {
    fn on_interrupt(&self) {
        let r = TIM2;
        critical_section::with(|cs| {
            let sr = r.sr().read();
            let dier = r.dier().read();
            // SR flags clear on writing 0; writing 1 leaves them alone, so
            // this clears exactly the flags handled here.
            r.sr().write_value(tim::regs::Sr(!sr.0));

            if sr.uif() {
                self.next_period(cs);
            }
            if sr.cc1if() {
                self.next_period(cs);
            }
            if sr.cc2if() && dier.cc2ie() {
                self.trigger_alarm(cs);
            }
        })
    }

    fn next_period(&self, cs: CriticalSection) {
        let period = self.period.load(Ordering::Relaxed) + 1;
        self.period.store(period, Ordering::Relaxed);
        let t = (period as u64) << 31;

        // The alarm is now within reach of CCR2 (set by `set_alarm`).
        let at = self.alarm.borrow(cs).get();
        if at < t + 0xC000_0000 {
            TIM2.dier().modify(|w| w.set_cc2ie(true));
        }
    }

    fn trigger_alarm(&self, cs: CriticalSection) {
        let mut next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now());
        while !self.set_alarm(cs, next) {
            next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now());
        }
    }

    /// Arms CC2 for `timestamp`. Returns false if that time has already
    /// passed, in which case the caller must process the queue again.
    fn set_alarm(&self, cs: CriticalSection, timestamp: u64) -> bool {
        let r = TIM2;
        self.alarm.borrow(cs).set(timestamp);

        let t = self.now();
        if timestamp <= t {
            r.dier().modify(|w| w.set_cc2ie(false));
            self.alarm.borrow(cs).set(u64::MAX);
            return false;
        }

        r.ccr2().write_value(tim::regs::Ccr2(timestamp as u32));

        // Arm now only if the match can't come a wrap early; otherwise
        // `next_period` arms it.
        let diff = timestamp - t;
        r.dier().modify(|w| w.set_cc2ie(diff < 0xC000_0000));

        // The counter may have passed CCR2 while we were setting it up.
        let t = self.now();
        if timestamp <= t {
            r.dier().modify(|w| w.set_cc2ie(false));
            self.alarm.borrow(cs).set(u64::MAX);
            return false;
        }
        true
    }
}

impl Driver for TimDriver {
    fn now(&self) -> u64 {
        let period = self.period.load(Ordering::Relaxed);
        compiler_fence(Ordering::Acquire);
        let counter = TIM2.cnt().read().0;
        calc_now(period, counter)
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            let mut queue = self.queue.borrow(cs).borrow_mut();
            if queue.schedule_wake(at, waker) {
                let mut next = queue.next_expiration(self.now());
                while !self.set_alarm(cs, next) {
                    next = queue.next_expiration(self.now());
                }
            }
        })
    }
}

#[interrupt]
fn TIM2() {
    DRIVER.on_interrupt();
}
