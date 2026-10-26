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
//! - `HardFault` and `DefaultHandler` that request the bootloader, so a
//!   crash lands in DFU instead of hanging.
//! - [`request_bootloader`] for the firmware's own bootloader keycode.
//! - With `panic-dfu`, a panic handler that keeps the panic message for the
//!   next boot ([`take_panic_message`]) and then requests the bootloader.
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

    @ Set the magic word and reset. Usable from any state.
    .global __boot_guard_reset_to_dfu
    .type __boot_guard_reset_to_dfu,%function
    .thumb_func
__boot_guard_reset_to_dfu:
    cpsid i
    ldr  r0, ={magic_addr}
    ldr  r1, ={magic}
    str  r1, [r0]
    dsb
    ldr  r0, =0xE000ED0C        @ SCB->AIRCR
    ldr  r1, =0x05FA0004        @ VECTKEY | SYSRESETREQ
    str  r1, [r0]
    dsb
1:  b    1b

    @ cortex-m-rt PROVIDEs these; ours take precedence.
    .global HardFault
    .type HardFault,%function
    .thumb_func
HardFault:
    b    __boot_guard_reset_to_dfu

    .global DefaultHandler
    .type DefaultHandler,%function
    .thumb_func
DefaultHandler:
    b    __boot_guard_reset_to_dfu

    .balign 4
    .global __boot_guard_pool
__boot_guard_pool:
    .ltorg
    "#,
    rom = const ROM_BOOTLOADER,
    magic_addr = const MAGIC_ADDR,
    magic = const MAGIC,
);

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
/// stack touches it. What the ROM bootloader does to this RAM is unknown, so
/// the record carries a check word and [`take_panic_message`] ignores a
/// damaged one.
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

fn panic_check(len: u32, msg: &[u8]) -> u32 {
    // FNV-1a over the length and the message.
    let mut h: u32 = 0x811C_9DC5;
    for b in len.to_le_bytes().iter().chain(msg) {
        h = (h ^ *b as u32).wrapping_mul(0x0100_0193);
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

/// Any panic lands in DFU instead of hanging, leaving its message for
/// [`take_panic_message`]. A fault while formatting it still ends in DFU,
/// through HardFault.
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
    request_bootloader()
}
