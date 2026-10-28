//! embassy-time driver on SysTick, 1 MHz tick.
//!
//! SysTick interrupts once a millisecond, as ChibiOS's periodic tick in QMK
//! (`CH_CFG_ST_TIMEDELTA 0`) does on this chip. `now()` is the millisecond
//! count plus how far SysTick has counted down into the current one; alarms
//! are checked on every tick, so a timer fires up to 1 ms late.
//!
//! The TIM2 driver this replaces armed compare alarms; on the board they
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

const TICKS_PER_MS: u64 = TICK_HZ / 1000;
const _: () = assert!(TICK_HZ.is_multiple_of(1000));
/// SysTick counts HCLK cycles, one wrap per millisecond.
const CYCLES_PER_MS: u32 = rcc::HCLK_HZ / 1000;
const _: () = assert!(CYCLES_PER_MS <= 1 << 24);

// repr(C) keeps `ms` at offset 0: the simulator tests read it.
#[repr(C)]
struct SysTickDriver {
    /// Milliseconds since `init`.
    ms: Mutex<Cell<u64>>,
    /// Next alarm time, `u64::MAX` when none.
    alarm: Mutex<Cell<u64>>,
    queue: Mutex<RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: SysTickDriver = SysTickDriver {
    ms: Mutex::new(Cell::new(0)),
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
    syst.set_reload(CYCLES_PER_MS - 1);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}

impl SysTickDriver {
    fn now_cs(&self, cs: CriticalSection) -> u64 {
        let mut ms = self.ms.borrow(cs).get();
        let mut cvr = SYST::get_current();
        // A wrap whose interrupt hasn't run yet (we're in a critical section
        // or a higher-priority handler): count it, and read again, as the
        // value above may be from before or after the wrap.
        if SCB::is_pendst_pending() {
            ms += 1;
            cvr = SYST::get_current();
        }
        let into_ms = (CYCLES_PER_MS - 1 - cvr.min(CYCLES_PER_MS - 1)) as u64;
        ms * TICKS_PER_MS + into_ms * TICKS_PER_MS / CYCLES_PER_MS as u64
    }

    fn on_tick(&self) {
        critical_section::with(|cs| {
            let ms = self.ms.borrow(cs);
            ms.set(ms.get() + 1);
            if self.alarm.borrow(cs).get() <= self.now_cs(cs) {
                self.arm_next(cs);
            }
        })
    }

    /// Wakes every expired timer and remembers when the next one is due.
    fn arm_next(&self, cs: CriticalSection) {
        let next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now_cs(cs));
        self.alarm.borrow(cs).set(next);
    }
}

impl Driver for SysTickDriver {
    fn now(&self) -> u64 {
        critical_section::with(|cs| self.now_cs(cs))
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            if self.queue.borrow(cs).borrow_mut().schedule_wake(at, waker) {
                self.arm_next(cs);
            }
        })
    }
}

#[exception]
fn SysTick() {
    DRIVER.on_tick();
}
