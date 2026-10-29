//! embassy-time driver on SysTick, 1 MHz tick, tickless.
//!
//! SysTick counts HCLK cycles down from its reload and interrupts at each
//! wrap. Each period is set to end at the next alarm, or after `MAX_PERIOD`
//! (the 24-bit counter's reach, 174 ms) when none is due, so an idle
//! keyboard takes a handful of interrupts a second, not ChibiOS's (and this
//! driver's first) one per millisecond. `now()` is the cycles counted up to
//! the current period's start plus how far SysTick is into it.
//!
//! An alarm earlier than the current period's end cuts it short: the cycles
//! so far are added up and SysTick restarts from a new reload (a write to
//! CVR). The few cycles between reading the counter and the restart are
//! lost, so each cut makes time a little slow (about 0.1 us); `now()`
//! never goes backwards. A period shorter than `MIN_PERIOD` isn't set: an
//! alarm that close fires that late.
//!
//! The TIM2 driver before SysTick armed compare alarms; on the board they
//! only fired at the 16-bit counter's wrap (a 1 ms timer took 65.5 ms, and
//! time went backwards), though Renode's timer matched them. QMK never uses
//! TIM2's compare channels.

use core::cell::{Cell, RefCell};
use core::task::Waker;

use cortex_m::peripheral::scb::SystemHandler;
use cortex_m::peripheral::syst::SystClkSource;
use cortex_m::peripheral::{SCB, SYST};
use cortex_m_rt::exception;
use critical_section::{CriticalSection, Mutex};
use embassy_time_driver::{Driver, TICK_HZ};
use embassy_time_queue_utils::Queue;

use crate::rcc;

/// SysTick counts HCLK cycles.
const CYCLES_PER_TICK: u32 = (rcc::HCLK_HZ as u64 / TICK_HZ) as u32;
const _: () = assert!(rcc::HCLK_HZ as u64 % TICK_HZ == 0);
/// Period lengths, in cycles: the 24-bit counter's longest, and 10 us.
const MAX_PERIOD: u32 = 1 << 24;
const MIN_PERIOD: u32 = rcc::HCLK_HZ / 100_000;
/// More ticks than the longest period: an alarm further off waits a
/// `MAX_PERIOD` first.
const MAX_TICKS: u64 = (MAX_PERIOD / CYCLES_PER_TICK + 1) as u64;

// repr(C) keeps `ticks` at offset 0, `frac` at 8 and `period` at 12: the
// simulator tests read them.
#[repr(C)]
struct SysTickDriver {
    /// Ticks from `init` to the start of the current period, and the cycles
    /// past that tick (below `CYCLES_PER_TICK`). Kept apart so that time
    /// takes 32-bit divisions only.
    ticks: Mutex<Cell<u64>>,
    frac: Mutex<Cell<u32>>,
    /// The current period's length in cycles (SysTick's reload + 1).
    period: Mutex<Cell<u32>>,
    /// The latest time `now_parts` returned: never less after.
    last: Mutex<Cell<u64>>,
    /// Next alarm time in ticks, `u64::MAX` when none.
    alarm: Mutex<Cell<u64>>,
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: SysTickDriver = SysTickDriver {
    ticks: Mutex::new(Cell::new(0)),
    frac: Mutex::new(Cell::new(0)),
    period: Mutex::new(Cell::new(MAX_PERIOD)),
    last: Mutex::new(Cell::new(0)),
    alarm: Mutex::new(Cell::new(u64::MAX)),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

/// Starts SysTick as the embassy-time base. Call after [`rcc::init`].
/// `prio` is SysTick's exception priority (upper 4 bits used).
pub fn init(prio: u8) {
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    unsafe { cp.SCB.set_priority(SystemHandler::SysTick, prio) };
    let syst = &mut cp.SYST;
    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(MAX_PERIOD - 1);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}

impl SysTickDriver {
    /// Now: the ticks since `init`, and the cycles past the last one.
    fn now_parts(&self, cs: CriticalSection) -> (u64, u32) {
        let period = self.period.borrow(cs).get();
        let mut cvr = SYST::get_current();
        // A wrap whose interrupt hasn't run yet (we're in a critical section
        // or a higher-priority handler): count it, and read again, as the
        // value above may be from before or after the wrap.
        let mut cycles = self.frac.borrow(cs).get();
        if SCB::is_pendst_pending() {
            cycles += period;
            cvr = SYST::get_current();
        }
        // At most 96 + 2 * 2^24: no overflow.
        cycles += (period - 1).saturating_sub(cvr);
        let ticks = self.ticks.borrow(cs).get() + (cycles / CYCLES_PER_TICK) as u64;
        let last = self.last.borrow(cs);
        if ticks < last.get() {
            return (last.get(), 0);
        }
        last.set(ticks);
        (ticks, cycles % CYCLES_PER_TICK)
    }

    fn now_cs(&self, cs: CriticalSection) -> u64 {
        self.now_parts(cs).0
    }

    /// Starts a new period of `period` cycles from now.
    fn restart(&self, cs: CriticalSection, now: (u64, u32), period: u32) {
        // SAFETY: SysTick is this driver's; we're in a critical section.
        let syst = unsafe { &mut cortex_m::Peripherals::steal().SYST };
        syst.set_reload(period - 1);
        syst.clear_current();
        // A wrap counted in `now` mustn't be counted again by the handler.
        SCB::clear_pendst();
        self.ticks.borrow(cs).set(now.0);
        self.frac.borrow(cs).set(now.1);
        self.period.borrow(cs).set(period);
    }

    /// Sets the period to end at the alarm (at most `MAX_PERIOD` on).
    fn program(&self, cs: CriticalSection) {
        let now = self.now_parts(cs);
        let left = self.alarm.borrow(cs).get().saturating_sub(now.0).min(MAX_TICKS) as u32;
        let cycles = (left * CYCLES_PER_TICK).saturating_sub(now.1);
        self.restart(cs, now, cycles.clamp(MIN_PERIOD, MAX_PERIOD));
    }

    fn on_tick(&self) {
        critical_section::with(|cs| {
            let cycles = self.frac.borrow(cs).get() + self.period.borrow(cs).get();
            let ticks = self.ticks.borrow(cs);
            ticks.set(ticks.get() + (cycles / CYCLES_PER_TICK) as u64);
            self.frac.borrow(cs).set(cycles % CYCLES_PER_TICK);
            if self.alarm.borrow(cs).get() <= self.now_cs(cs) {
                self.next_alarm(cs);
            }
            self.program(cs);
        })
    }

    /// Wakes every expired timer and remembers when the next one is due.
    fn next_alarm(&self, cs: CriticalSection) {
        let next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now_cs(cs));
        self.alarm.borrow(cs).set(next);
    }

    /// The current period's end, in ticks (rounded down).
    fn period_end(&self, cs: CriticalSection) -> u64 {
        let cycles = self.frac.borrow(cs).get() + self.period.borrow(cs).get();
        self.ticks.borrow(cs).get() + (cycles / CYCLES_PER_TICK) as u64
    }
}

impl Driver for SysTickDriver {
    fn now(&self) -> u64 {
        critical_section::with(|cs| self.now_cs(cs))
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            if self.queue.borrow(cs).borrow_mut().schedule_wake(at, waker) {
                self.next_alarm(cs);
                // Sooner than the period ends: cut it short.
                if self.alarm.borrow(cs).get() < self.period_end(cs) {
                    self.program(cs);
                }
            }
        })
    }
}

#[exception]
fn SysTick() {
    DRIVER.on_tick();
}
