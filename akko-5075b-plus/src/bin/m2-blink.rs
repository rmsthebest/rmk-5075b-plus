//! M2 image: the HAL core on embassy. 96 MHz clocks, the TIM2 time base and
//! an EXTI wait, observable on the Caps Lock LED (C5, active low):
//! - blinks at 1 Hz for 10 s, then resets into DFU;
//! - pressing Esc returns to DFU at once (EXTI on row A0, column C0 held low);
//! - time going backwards panics, which also lands in DFU.

#![no_std]
#![no_main]

use boot_guard as _;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use wb32_hal::exti::ExtiInput;
use wb32_hal::gpio::{Level, Output, Pins, Pull};
use wb32_hal::{rcc, time_driver};

const BLINK_HALF_PERIOD: Duration = Duration::from_millis(500);
const RUN_TIME_BLINKS: u32 = 20; // 20 x 500 ms = 10 s

/// NVIC priorities (upper 4 bits): the time base above everything else.
const PRIO_TIME: u8 = 0x40;
const PRIO_EXTI: u8 = 0x80;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    rcc::init();
    time_driver::init(PRIO_TIME);
    let p = Pins::take().unwrap();

    let mut caps = Output::new(p.PC5, Level::High); // off
    // Esc is row A0 x column C0: hold the column low, wait for the row to fall.
    let _esc_col = Output::new(p.PC0, Level::Low);
    let mut esc = ExtiInput::new(p.PA0, Pull::Up, PRIO_EXTI);

    let blink = async {
        let mut last = Instant::now();
        for _ in 0..RUN_TIME_BLINKS {
            caps.toggle();
            Timer::after(BLINK_HALF_PERIOD).await;
            let now = Instant::now();
            assert!(now >= last, "time went backwards");
            last = now;
        }
    };

    match select(blink, esc.wait_for_falling_edge()).await {
        Either::First(()) | Either::Second(()) => boot_guard::request_bootloader(),
    }
}
