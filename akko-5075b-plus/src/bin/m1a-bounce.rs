//! M1 image (a): build with `--features always-dfu`. boot-guard's
//! `__pre_init` enters the ROM bootloader on every boot, so `main` never runs.

#![no_std]
#![no_main]

use boot_guard as _;
use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    boot_guard::request_bootloader()
}
