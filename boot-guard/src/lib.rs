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

/// Any panic lands in DFU instead of hanging.
#[cfg(feature = "panic-dfu")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    request_bootloader()
}
