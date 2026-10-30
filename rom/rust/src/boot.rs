//! Leaving the bootloader: RESET (ROM 0x1FFFEC90-0x1FFFECC5) and GO (ROM
//! 0x1FFFECC6-0x1FFFED19). Both answer first, wait 10 ms, take USB down and
//! pull D+ low (usb::disconnect), then wait their argument in milliseconds.

use crate::clock::delay;
use crate::dfu::{ARG_ERROR, FLASH, region};
use crate::mmio::*;
use crate::usb;
use core::arch::asm;
use wb32_pac::RCC;

/// 10 ms and 1 ms of 48 MHz core clocks.
const TEN_MS: u32 = 480_000;
const ONE_MS: u32 = 48_000;

fn leave(ms: u32) {
    usb::send(2);
    delay(TEN_MS);
    usb::deinit();
    usb::disconnect();
    for _ in 0..ms {
        delay(ONE_MS);
    }
}

/// RESET: arg0 is the wait; then SYSRESETREQ.
pub fn reset(ms: u32) -> ! {
    leave(ms);
    w32(SCB_AIRCR, 0x05FA_0004);
    loop {}
}

/// GO: arg0 is the image (its vector table), which must be in main flash;
/// arg1 is the wait. The main clock goes back to MHSI with the PLL left
/// running, VTOR to 0 (the image's own address only through the boot alias,
/// so the image should set VTOR itself), MSP from the image, and a call to
/// its reset vector.
///
/// Returns ARG_ERROR for the command loop to send if arg0 isn't in flash.
/// If the image returns, the ROM goes back to its command loop on the
/// image's stack; this translation doesn't (the call doesn't return).
pub fn go(addr: u32, ms: u32) -> u8 {
    if region(addr) != FLASH {
        return ARG_ERROR;
    }
    leave(ms);
    w32(RCC.mainclksrc(), 0);
    w32(RCC.mainclkuen(), 1);
    w32(SCB_VTOR, 0);
    unsafe {
        let sp = (addr as *const u32).read_volatile();
        let pc = (addr as *const u32).add(1).read_volatile();
        asm!("msr msp, {sp}", "blx {pc}", sp = in(reg) sp, pc = in(reg) pc, options(noreturn));
    }
}
