//! Reset-time escape hatch for the Akko 5075B Plus (WB32FQ95).
//!
//! The case can't be opened, so the ROM DFU bootloader at `0x1FFF_E000` is
//! only reachable through code in our own image. This crate provides:
//!
//! - `__pre_init` (runs from cortex-m-rt's `Reset`, before RAM init and
//!   before any clock setup):
//!   1. If the RAM magic word is set, clear it and enter the ROM bootloader.
//!      Same protocol as QMK's `platforms/chibios/bootloaders/wb32_dfu.c`, so
//!      QMK and Rust images can hand over to each other.
//!   2. Read matrix position (0,0) (Esc: column C0 driven low, row A0 read
//!      with pull-up). If Esc is held, set the magic word and reset, so the
//!      ROM is always entered from a clean reset state.
//! - `HardFault` and `DefaultHandler` that follow the panic handler's crash
//!   policy without touching the stack (a stack overflow may be what
//!   faulted): they leave "HardFault" for [`take_panic_message`] and reset
//!   into the firmware, or enter DFU when a crash came before since
//!   [`mark_stable`].
//! - [`request_bootloader`] for the firmware's own bootloader keycode.
//!   Every way into DFU clears the crash count: DFU on purpose isn't a
//!   crash, and the image flashed next starts clean.
//! - With `panic-dfu`, a panic handler that keeps the panic message for the
//!   next boot ([`take_panic_message`]) and resets into the firmware; a
//!   second panic before the firmware called [`mark_stable`] requests the
//!   bootloader instead, so a crash loop ends in DFU.
//!
//! Everything that runs at reset is plain assembly that touches only
//! peripheral registers and the hardware-provided stack.

#![no_std]

use core::arch::global_asm;

/// Last word of the 28K RAM that QMK links for (`__ram0_end__ - 4`).
pub const MAGIC_ADDR: u32 = 0x2000_6FFC;
pub const MAGIC: u32 = 0xDEAD_BEEF;
pub const ROM_BOOTLOADER: u32 = 0x1FFF_E000;

// Shared pieces: ROM entry and the fault handlers.
global_asm!(
    r#"
    .section .text.__boot_guard,"ax",%progbits
    .syntax unified
    .thumb

    @ Enter the ROM bootloader. Only called from __pre_init, i.e. with the
    @ chip still in its reset state (same sequence as QMK).
    .global __boot_guard_enter_rom
    .type __boot_guard_enter_rom,%function
    .thumb_func
__boot_guard_enter_rom:
    movs r0, #0
    msr  CONTROL, r0
    isb
    ldr  r0, ={rom}
    ldr  r1, [r0]
    msr  MSP, r1
    cpsie i
    ldr  r1, [r0, #4]
    bx   r1

    @ Clear the crash count, set the magic word and reset. Usable from any
    @ state.
    .global __boot_guard_reset_to_dfu
    .type __boot_guard_reset_to_dfu,%function
    .thumb_func
__boot_guard_reset_to_dfu:
    cpsid i
    ldr  r0, ={count_addr}
    ldr  r1, ={crash_magic}
    movs r2, #0
    str  r1, [r0]
    str  r2, [r0, #4]
    mvns r2, r2
    str  r2, [r0, #8]
    ldr  r0, ={magic_addr}
    ldr  r1, ={magic}
    str  r1, [r0]
    dsb
    ldr  r0, =0xE000ED0C        @ SCB->AIRCR
    ldr  r1, =0x05FA0004        @ VECTKEY | SYSRESETREQ
    str  r1, [r0]
    dsb
1:  b    1b

    @ cortex-m-rt PROVIDEs these; ours take precedence. The panic
    @ handler's crash policy, with no stack: the record "HardFault", then
    @ the crash count + 1 and a reset, or after CRASHES_BEFORE_DFU, DFU.
    .global HardFault
    .type HardFault,%function
    .thumb_func
HardFault:
    .global DefaultHandler
    .type DefaultHandler,%function
    .thumb_func
DefaultHandler:
    cpsid i
    @ The record's length, check and message, then its magic word.
    ldr  r0, ={record_addr}
    adr  r1, 4f
    movs r2, #0
1:  ldr  r3, [r1, r2]
    adds r2, #4
    str  r3, [r0, r2]
    cmp  r2, #{fault_words} * 4
    bne  1b
    ldr  r1, ={panic_magic}
    str  r1, [r0]
    @ The crash count, 0 if its words don't check out.
    ldr  r0, ={count_addr}
    ldr  r1, ={crash_magic}
    movs r3, #0
    ldr  r2, [r0]
    cmp  r2, r1
    bne  2f
    ldr  r2, [r0, #8]
    mvns r2, r2
    ldr  r3, [r0, #4]
    cmp  r2, r3
    beq  2f
    movs r3, #0
2:  cmp  r3, #{before}
    bhs  __boot_guard_reset_to_dfu
    adds r3, #1
    str  r1, [r0]
    str  r3, [r0, #4]
    mvns r3, r3
    str  r3, [r0, #8]
    dsb
    ldr  r0, =0xE000ED0C        @ SCB->AIRCR
    ldr  r1, =0x05FA0004        @ VECTKEY | SYSRESETREQ
    str  r1, [r0]
    dsb
3:  b    3b

    .balign 4
    .global __boot_guard_pool
__boot_guard_pool:
    .ltorg
    @ The fault record after its magic word (FAULT_RECORD), here rather
    @ than in .rodata so the reviewed listing holds its bytes and no
    @ layout-dependent address.
    .balign 4
4:  .word {r0}, {r1}, {r2}, {r3}, {r4}
    "#,
    rom = const ROM_BOOTLOADER,
    magic_addr = const MAGIC_ADDR,
    magic = const MAGIC,
    count_addr = const CRASH_COUNT_ADDR,
    crash_magic = const CRASH_MAGIC,
    before = const CRASHES_BEFORE_DFU,
    record_addr = const PANIC_RECORD_ADDR,
    panic_magic = const PANIC_MAGIC,
    fault_words = const FAULT_WORDS,
    r0 = const FAULT_RECORD[0],
    r1 = const FAULT_RECORD[1],
    r2 = const FAULT_RECORD[2],
    r3 = const FAULT_RECORD[3],
    r4 = const FAULT_RECORD[4],
);

/// The fault handlers' panic record after its magic word: length, check,
/// message (padded to a word).
const FAULT_MSG: &[u8] = b"HardFault";
const FAULT_WORDS: usize = 2 + FAULT_MSG.len().div_ceil(4);

const _: () = assert!(FAULT_WORDS == 5, "the asm's .word list has 5 operands");

const FAULT_RECORD: [u32; FAULT_WORDS] = {
    let len = FAULT_MSG.len() as u32;
    let mut w = [0; FAULT_WORDS];
    w[0] = len;
    w[1] = panic_check(len, FAULT_MSG);
    let mut i = 0;
    while i < FAULT_MSG.len() {
        w[2 + i / 4] |= (FAULT_MSG[i] as u32) << (8 * (i % 4));
        i += 1;
    }
    w
};

// First bring-up image: go straight back to the ROM on every boot, before
// touching any peripheral. Proves the vector table, linker layout and ROM
// entry with nothing else in the way.
#[cfg(feature = "always-dfu")]
global_asm!(
    r#"
    .section .text.__pre_init,"ax",%progbits
    .syntax unified
    .thumb
    .global __pre_init
    .type __pre_init,%function
    .thumb_func
__pre_init:
    b    __boot_guard_enter_rom
    "#
);

#[cfg(not(feature = "always-dfu"))]
global_asm!(
    r#"
    .section .text.__pre_init,"ax",%progbits
    .syntax unified
    .thumb
    .global __pre_init
    .type __pre_init,%function
    .thumb_func
__pre_init:
    @ 1. Magic word from a previous request_bootloader() (Rust or QMK).
    ldr  r0, ={magic_addr}
    ldr  r1, [r0]
    ldr  r2, ={magic}
    cmp  r1, r2
    bne  1f
    movs r1, #0
    str  r1, [r0]
    b    __boot_guard_enter_rom
1:
    push {{r4, r5, lr}}

    @ 2. Clocks for GPIOA/GPIOC, in the order ChibiOS's wb32_clock_init()
    @    and board.c use: APB1 clock source (inside the ANCTL key window),
    @    then the APB1 bus matrix, then the GPIO ports.
    ldr  r0, =0x40010000        @ PWR
    movs r1, #0x03
    str  r1, [r0, #0x28]        @ ANAKEY1
    movs r1, #0x0C
    str  r1, [r0, #0x2C]        @ ANAKEY2
    ldr  r2, =0x40010C00        @ RCC
    movs r1, #0x80
    str  r1, [r2, #0x1C]        @ APB1PRE = SRCEN, APB1 = MAINCLK / 1
    movs r1, #0
    str  r1, [r0, #0x28]
    str  r1, [r0, #0x2C]
    ldr  r1, [r2, #0x48]        @ APB1ENR
    ldr  r3, =0x80A0            @ BMX1EN | GPIOCEN | GPIOAEN
    orrs r1, r3
    str  r1, [r2, #0x48]
    ldr  r1, [r2, #0x48]        @ read back, as rccEnableAPB1() does

    @ 3. Esc column C0: output low. Config writes only reach pins whose
    @    CFGMSK bit is clear, so unmask pin 0 only (save the old mask).
    ldr  r0, =0x40000800        @ GPIOC
    ldr  r4, [r0, #0x30]        @ saved GPIOC->CFGMSK
    ldr  r1, =0x00010000
    str  r1, [r0, #0x18]        @ BSRR: C0 low before it becomes an output
    ldr  r1, =0xFFFE
    str  r1, [r0, #0x30]        @ CFGMSK = ~(1 << 0)
    movs r1, #0
    str  r1, [r0, #0x04]        @ OTYPER: push-pull
    ldr  r1, =0x55555555
    str  r1, [r0, #0x00]        @ MODER: output

    @    Esc row A0: input with pull-up.
    ldr  r0, =0x40000000        @ GPIOA
    ldr  r5, [r0, #0x30]        @ saved GPIOA->CFGMSK
    ldr  r1, =0xFFFE
    str  r1, [r0, #0x30]
    movs r1, #0
    str  r1, [r0, #0x00]        @ MODER: input
    ldr  r1, =0x55555555
    str  r1, [r0, #0x0C]        @ PUPDR: pull-up

    @    Let the pull-up settle (bounded loop, about 1 ms at reset clock).
    ldr  r1, =4000
2:  subs r1, #1
    bne  2b

    ldr  r3, [r0, #0x10]        @ GPIOA->IDR

    @ 4. Put both pins back to their reset state (input, no pull).
    movs r1, #0
    str  r1, [r0, #0x0C]        @ A0 PUPDR: none
    str  r5, [r0, #0x30]        @ restore GPIOA->CFGMSK
    ldr  r0, =0x40000800
    str  r1, [r0, #0x00]        @ C0 MODER: input
    str  r4, [r0, #0x30]        @ restore GPIOC->CFGMSK

    @ 5. A0 low means Esc is held: reset into the bootloader.
    lsls r3, r3, #31            @ IDR bit 0 -> bit 31, Z set if low
    bne  3f
    b    __boot_guard_reset_to_dfu
3:
    pop  {{r4, r5, pc}}

    .balign 4
    .global __boot_guard_pre_init_pool
__boot_guard_pre_init_pool:
    .ltorg
    "#,
    magic_addr = const MAGIC_ADDR,
    magic = const MAGIC,
);

unsafe extern "C" {
    fn __boot_guard_reset_to_dfu() -> !;
}

/// Reset into the ROM DFU bootloader (sets the magic word, then resets).
pub fn request_bootloader() -> ! {
    unsafe { __boot_guard_reset_to_dfu() }
}

/// Where the panic handler leaves the panic message for the next boot: just
/// above the 28K the images link (and QMK uses), so neither RAM init nor the
/// stack touches it. The ROM bootloader uses only 0x2000_0000-0x2000_044F
/// (rom/notes.md), so the record survives DFU; it still carries a check
/// word, and [`take_panic_message`] ignores a damaged one.
pub const PANIC_RECORD_ADDR: u32 = 0x2000_7000;
pub const PANIC_MSG_MAX: usize = 120;
const PANIC_MAGIC: u32 = 0x504E_4943; // "PNIC"

#[repr(C)]
struct PanicRecord {
    magic: u32,
    len: u32,
    check: u32,
    msg: [u8; PANIC_MSG_MAX],
}

fn panic_record() -> *mut PanicRecord {
    PANIC_RECORD_ADDR as *mut PanicRecord
}

const fn panic_check(len: u32, msg: &[u8]) -> u32 {
    // FNV-1a over the length and the message.
    const fn step(h: u32, b: u8) -> u32 {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    }
    let mut h: u32 = 0x811C_9DC5;
    let len = len.to_le_bytes();
    let mut i = 0;
    while i < 4 {
        h = step(h, len[i]);
        i += 1;
    }
    let mut i = 0;
    while i < msg.len() {
        h = step(h, msg[i]);
        i += 1;
    }
    h
}

/// Returns the message of the panic that ended the previous run, at most
/// once: the record is cleared on the first call.
pub fn take_panic_message(buf: &mut [u8; PANIC_MSG_MAX]) -> Option<&str> {
    let r = panic_record();
    unsafe {
        if (&raw const (*r).magic).read_volatile() != PANIC_MAGIC {
            return None;
        }
        (&raw mut (*r).magic).write_volatile(0);
        let len = (&raw const (*r).len).read_volatile();
        let check = (&raw const (*r).check).read_volatile();
        if len as usize > PANIC_MSG_MAX {
            return None;
        }
        let len = len as usize;
        for (i, b) in buf[..len].iter_mut().enumerate() {
            *b = (&raw const (*r).msg[i]).read_volatile();
        }
        if panic_check(len as u32, &buf[..len]) != check {
            return None;
        }
        // The writer truncates on a byte boundary; keep the valid prefix.
        Some(match core::str::from_utf8(&buf[..len]) {
            Ok(s) => s,
            Err(e) => core::str::from_utf8(&buf[..e.valid_up_to()]).unwrap_or(""),
        })
    }
}

/// Panics since the firmware last called [`mark_stable`], kept across the
/// reset after the panic record: magic, count, and the count's complement.
const CRASH_COUNT_ADDR: u32 = 0x2000_7100;
const CRASH_MAGIC: u32 = 0x4352_5348; // "CRSH"
/// A panic with this many before it (since [`mark_stable`]) goes to DFU.
pub const CRASHES_BEFORE_DFU: u32 = 1;

fn crash_count_ptr() -> *mut [u32; 3] {
    CRASH_COUNT_ADDR as *mut [u32; 3]
}

/// Panics since the last [`mark_stable`] (0 after a power-on: the words
/// don't check out).
pub fn crash_count() -> u32 {
    let w = unsafe { crash_count_ptr().read_volatile() };
    if w[0] == CRASH_MAGIC && w[2] == !w[1] {
        w[1]
    } else {
        0
    }
}

fn set_crash_count(n: u32) {
    unsafe { crash_count_ptr().write_volatile([CRASH_MAGIC, n, !n]) };
}

/// The firmware has run long enough to count as working: the next panic
/// resets into it again instead of going to DFU.
pub fn mark_stable() {
    set_crash_count(0);
}

#[cfg(feature = "panic-dfu")]
struct RecordWriter {
    len: usize,
}

#[cfg(feature = "panic-dfu")]
impl core::fmt::Write for RecordWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let r = panic_record();
        for b in s.bytes() {
            if self.len == PANIC_MSG_MAX {
                return Err(core::fmt::Error);
            }
            unsafe { (&raw mut (*r).msg[self.len]).write_volatile(b) };
            self.len += 1;
        }
        Ok(())
    }
}

/// A panic leaves its message for [`take_panic_message`] and resets into the
/// firmware, or, when it follows another panic since [`mark_stable`],
/// enters DFU. A fault while formatting the message is a crash too, through
/// HardFault, and leaves its own record.
#[cfg(feature = "panic-dfu")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    // No interrupt handler runs on a half-written record.
    unsafe { core::arch::asm!("cpsid i", options(nomem, nostack, preserves_flags)) };
    let r = panic_record();
    let mut w = RecordWriter { len: 0 };
    // One line: "file:line:col: message".
    if let Some(loc) = info.location() {
        let _ = write!(w, "{loc}: ");
    }
    let _ = write!(w, "{}", info.message());
    let mut msg = [0u8; PANIC_MSG_MAX];
    for (i, b) in msg[..w.len].iter_mut().enumerate() {
        *b = unsafe { (&raw const (*r).msg[i]).read_volatile() };
    }
    unsafe {
        (&raw mut (*r).len).write_volatile(w.len as u32);
        (&raw mut (*r).check).write_volatile(panic_check(w.len as u32, &msg[..w.len]));
        (&raw mut (*r).magic).write_volatile(PANIC_MAGIC);
    }
    let crashes = crash_count();
    if crashes >= CRASHES_BEFORE_DFU {
        request_bootloader()
    }
    set_crash_count(crashes + 1);
    // SYSRESETREQ: back into the firmware (boot-guard's checks run first).
    unsafe {
        core::arch::asm!("dsb", options(nomem, nostack, preserves_flags));
        (0xE000_ED0C as *mut u32).write_volatile(0x05FA_0004);
        core::arch::asm!("dsb", options(nomem, nostack, preserves_flags));
    }
    loop {}
}
