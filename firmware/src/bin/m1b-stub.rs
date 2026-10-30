//! M1 image (b): the full boot-guard stub, then blink the Caps Lock LED
//! (C5, active low) for about 30 s and reset back into DFU.

#![no_std]
#![no_main]

use akko_boot_guard as _;
// Device interrupt vectors (all DefaultHandler -> DFU).
use cortex_m_rt::entry;
use wb32_pac as _;

const GPIOC: u32 = 0x4000_0800;
const MODER: u32 = 0x00;
const OTYPER: u32 = 0x04;
const BSRR: u32 = 0x18;
const CFGMSK: u32 = 0x30;
const CAPS_LED: u32 = 5;

fn reg(addr: u32) -> *mut u32 {
    addr as *mut u32
}

#[entry]
fn main() -> ! {
    // boot-guard already enabled the GPIOC clock. Running on the reset
    // clock (8 MHz MHSI); the delays below are rough on purpose.
    unsafe {
        reg(GPIOC + BSRR).write_volatile(1 << CAPS_LED); // high = LED off
        reg(GPIOC + CFGMSK).write_volatile(!(1u32 << CAPS_LED) & 0xFFFF);
        reg(GPIOC + OTYPER).write_volatile(0);
        reg(GPIOC + MODER).write_volatile(0x5555_5555);
    }

    // 30 blinks at roughly 1 Hz.
    for _ in 0..30 {
        unsafe { reg(GPIOC + BSRR).write_volatile(1 << (CAPS_LED + 16)) }; // on
        cortex_m::asm::delay(4_000_000);
        unsafe { reg(GPIOC + BSRR).write_volatile(1 << CAPS_LED) }; // off
        cortex_m::asm::delay(4_000_000);
    }

    akko_boot_guard::request_bootloader()
}
