//! The WB32FQ95's ROM bootloader (system memory 0x1FFFE000-0x1FFFEFFF,
//! version 4), rewritten in Rust from its disassembly to show what it does.
//! Each function names the ROM range it stands for; register writes are in
//! the ROM's order and at its widths, odd parts kept, so that its
//! write traces match the ROM's (README.md). It runs in the sim only and
//! never goes on the board.
//!
//! The ROM starts the clocks, holds D+ low for 1.5 s, enumerates as
//! 342d:dfa0 (a vendor-class device with one bulk endpoint pair, not USB
//! DFU), and then serves six commands on EP1 for ever: get info, erase,
//! write a page, read, reset, go.

#![no_std]
#![no_main]
// The ROM's own shapes: its `b .` hangs, and `x > 16 ? x - 16 : 0`.
#![allow(clippy::empty_loop, clippy::implicit_saturating_sub)]

mod boot;
mod clock;
mod desc;
mod dfu;
mod ep0;
mod flash;
mod mmio;
mod ram;
mod usb;

use core::arch::naked_asm;
use mmio::{SCB_AIRCR, w32};

/// ROM 0x1FFFE940-0x1FFFEAF1, main's start-up: clocks, 1.5 s of
/// disconnect, USB on. Then the command loop.
extern "C" fn rom_main() -> ! {
    clock::start();
    usb::disconnect();
    for _ in 0..15 {
        // 100 ms.
        clock::delay(4_800_000);
    }
    usb::init();
    usb::pins();
    dfu::run()
}

/// ROM 0x1FFFE090 then 0x1FFFE07C: interrupts off, the stack pointer
/// reloaded (so it doesn't matter what the caller left in it), RAM set up,
/// main.
#[unsafe(naked)]
#[unsafe(no_mangle)]
unsafe extern "C" fn reset() -> ! {
    naked_asm!(
        "cpsid i",
        "ldr sp, ={top}",
        "bl {init}",
        "b {main}",
        top = const ram::STACK_TOP,
        init = sym init,
        main = sym rom_main,
    )
}

unsafe extern "C" {
    static _sidata: u32;
    static mut _sdata: u32;
    static mut _edata: u32;
    static mut _sbss: u32;
    static mut _ebss: u32;
}

/// ROM 0x1FFFE0E0-0x1FFFE0F7 and its table at 0x1FFFEFC8: copy .data
/// (0x18 bytes from 0x1FFFEFE8, word copy 0x1FFFE8AC) and zero the rest
/// (word zero 0x1FFFE8BC). The ROM zeroes 0x438 bytes, up to its stack top,
/// stack included, which its init can do as it never pushes; this one zeroes
/// .bss only.
unsafe extern "C" fn init() {
    unsafe {
        let mut src = &raw const _sidata;
        let mut dst = &raw mut _sdata;
        while dst < &raw mut _edata {
            dst.write_volatile(src.read());
            dst = dst.add(1);
            src = src.add(1);
        }
        let mut dst = &raw mut _sbss;
        while dst < &raw mut _ebss {
            dst.write_volatile(0);
            dst = dst.add(1);
        }
    }
}

/// ROM 0x1FFFE098: a hard fault resets the chip.
unsafe extern "C" fn hard_fault() -> ! {
    w32(SCB_AIRCR, 0x05FA_0004);
    loop {}
}

/// ROM 0x1FFFE096 and 0x1FFFE0A0-0x1FFFE0AF: the other handlers each spin
/// on their own `b .` (and every interrupt but USB shares 0x1FFFE0AE).
unsafe extern "C" fn hang() -> ! {
    loop {}
}

#[derive(Clone, Copy)]
#[repr(C)]
union Vector {
    handler: unsafe extern "C" fn() -> !,
    irq: unsafe extern "C" fn(),
    word: u32,
}

#[repr(transparent)]
struct Vectors([Vector; 31]);

unsafe impl Sync for Vectors {}

const HANG: Vector = Vector { handler: hang };
const NONE: Vector = Vector { word: 0 };

/// ROM 0x1FFFE000-0x1FFFE07B: 16 exceptions and IRQs 0-14, up to USB.
#[unsafe(link_section = ".vector_table")]
#[used]
static VECTORS: Vectors = Vectors([
    Vector { word: ram::STACK_TOP },
    Vector { handler: reset },
    HANG, // NMI
    Vector { handler: hard_fault },
    HANG, // MemManage
    HANG, // BusFault
    HANG, // UsageFault
    // Reserved: the low byte is the bootloader version GET_INFO reports.
    Vector { word: 0xFFFF_0004 },
    NONE,
    NONE,
    NONE,
    HANG, // SVCall
    HANG, // DebugMonitor
    NONE,
    HANG, // PendSV
    HANG, // SysTick
    HANG, // IRQ 0-13
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    HANG,
    Vector { irq: usb::irq }, // IRQ 14, USB
]);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
