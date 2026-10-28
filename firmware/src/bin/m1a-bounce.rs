//! M1 image (a): build with `--features always-dfu`. boot-guard's
//! `__pre_init` enters the ROM bootloader on every boot, so `main` never runs.

#![no_std]
#![no_main]

use akko_boot_guard as _;
// Device interrupt vectors (all DefaultHandler -> DFU).
use wb32_pac as _;
use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    akko_boot_guard::request_bootloader()
}
