//! A software watchdog: TIM2's interrupt (wb32_hal::tick) checks every
//! 250 ms that the firmware is still getting on, and panics if it has
//! stopped for `STUCK_AFTER`. boot-guard's panic handler then keeps the
//! message ("watchdog: ... stuck", read over Vial channel 0xD2) and resets;
//! a second time before the image is stable, it goes to DFU, as any panic.
//!
//! Two counters must move:
//! - `scan_alive`, every scan pass (1 ms while a key is down): the
//!   executor runs and the keys are read. While the scan is parked
//!   (`scan_parked`, no key down) it counts as moving.
//! - `MAIN_ALIVE`, every 100 ms from `heartbeat`, a future in main's join:
//!   the time queue (SysTick) works.
//!
//! What it can't catch: a hang with interrupts off (in a critical section)
//! or a higher-priority interrupt that never ends. Only the hardware IWDG
//! could, and it's unproven on this chip: it may survive the reset into
//! the ROM bootloader, or run in stop mode.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_time::Timer;

/// The check's period, and how many in a row without progress panic.
const PERIOD_MS: u32 = 250;
const STUCK_AFTER: u32 = 8;
/// Above every other interrupt (src/main.rs PRIO_*).
const PRIO: u8 = 0x20;

static SCAN_ALIVE: AtomicU32 = AtomicU32::new(0);
static MAIN_ALIVE: AtomicU32 = AtomicU32::new(0);
static SCAN_PARKED: AtomicBool = AtomicBool::new(false);
/// The values the last check saw, and the checks since they last moved.
static SEEN: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
static STUCK: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// The scan made a pass.
pub fn scan_alive() {
    SCAN_ALIVE.store(SCAN_ALIVE.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

/// The scan parked (waiting for a key) or went back to its passes.
pub fn scan_parked(parked: bool) {
    SCAN_PARKED.store(parked, Ordering::Relaxed);
}

/// Starts the checks: once everything that runs is about to run (a long
/// start-up, such as storage's first format, isn't watched).
pub fn start() {
    wb32_hal::tick::start(PERIOD_MS, PRIO, check);
}

/// Bumps `MAIN_ALIVE` every 100 ms.
pub async fn heartbeat() -> ! {
    loop {
        MAIN_ALIVE.store(MAIN_ALIVE.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
        Timer::after_millis(100).await;
    }
}

fn check() {
    for (i, (counter, name)) in [(&SCAN_ALIVE, "scan"), (&MAIN_ALIVE, "main")].into_iter().enumerate() {
        let v = counter.load(Ordering::Relaxed);
        if v != SEEN[i].load(Ordering::Relaxed) || i == 0 && SCAN_PARKED.load(Ordering::Relaxed) {
            SEEN[i].store(v, Ordering::Relaxed);
            STUCK[i].store(0, Ordering::Relaxed);
            continue;
        }
        let n = STUCK[i].load(Ordering::Relaxed) + 1;
        STUCK[i].store(n, Ordering::Relaxed);
        if n >= STUCK_AFTER {
            panic!("watchdog: {name} stuck");
        }
    }
}
