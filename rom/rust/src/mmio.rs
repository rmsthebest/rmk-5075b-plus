//! Register access at the ROM's widths. The PAC names the registers; these
//! write the raw values the ROM writes, as 32-, 16- or 8-bit accesses like
//! its str/strh/strb, and also reach the registers the PAC marks read-only
//! or doesn't have.

use core::arch::asm;
use wb32_pac::common::{Access, RW, Reg};

/// A register the PAC doesn't name, by address.
pub const fn reg(addr: usize) -> Reg<u32, RW> {
    unsafe { Reg::from_ptr(addr as *mut u32) }
}

#[inline(always)]
pub fn w32<T: Copy, A: Access>(r: Reg<T, A>, v: u32) {
    unsafe { (r.as_ptr() as *mut u32).write_volatile(v) }
}

#[inline(always)]
pub fn r32<T: Copy, A: Access>(r: Reg<T, A>) -> u32 {
    unsafe { (r.as_ptr() as *const u32).read_volatile() }
}

/// Read, change, write back: the ROM's ldr/orr/str.
#[inline(always)]
pub fn m32<T: Copy, A: Access>(r: Reg<T, A>, f: impl FnOnce(u32) -> u32) {
    w32(r, f(r32(r)))
}

#[inline(always)]
pub fn r16<T: Copy, A: Access>(r: Reg<T, A>) -> u16 {
    unsafe { (r.as_ptr() as *const u16).read_volatile() }
}

#[inline(always)]
pub fn w8<T: Copy, A: Access>(r: Reg<T, A>, v: u8) {
    unsafe { (r.as_ptr() as *mut u8).write_volatile(v) }
}

#[inline(always)]
pub fn r8<T: Copy, A: Access>(r: Reg<T, A>) -> u8 {
    unsafe { (r.as_ptr() as *const u8).read_volatile() }
}

#[inline(always)]
pub fn m8<T: Copy, A: Access>(r: Reg<T, A>, f: impl FnOnce(u8) -> u8) {
    w8(r, f(r8(r)))
}

// Cortex-M core registers.
pub const SCB_ICSR: Reg<u32, RW> = reg(0xE000_ED04);
pub const SCB_VTOR: Reg<u32, RW> = reg(0xE000_ED08);
pub const SCB_AIRCR: Reg<u32, RW> = reg(0xE000_ED0C);
pub const SYST_CSR: Reg<u32, RW> = reg(0xE000_E010);
pub const SYST_RVR: Reg<u32, RW> = reg(0xE000_E014);
pub const SYST_CVR: Reg<u32, RW> = reg(0xE000_E018);
pub const NVIC_ISER0: Reg<u32, RW> = reg(0xE000_E100);
pub const NVIC_ICER0: Reg<u32, RW> = reg(0xE000_E180);
pub const NVIC_ICER1: Reg<u32, RW> = reg(0xE000_E184);
/// NVIC_IPR byte 14, the USB interrupt's priority.
pub const NVIC_IPR14: Reg<u32, RW> = reg(0xE000_E40E);

// What the PAC doesn't have.
/// RCC+0x104, not in the reference manual. Non-zero turns the IWDG on
/// (notes.md "Watchdog"); what sets it is an open question.
pub const RCC_IWDG_ON: Reg<u32, RW> = reg(0x4001_0D04);
/// ANCTL+0x34, between MHSISR and FHSIENR: bits 20-27 take an FHSI trim.
pub const ANCTL_FHSITRIM: Reg<u32, RW> = reg(0x4001_0434);
/// 0x4000B800-0x4000BBFF is "Reserved" in the manual. RCC_RNGCLKENR clocks
/// it, so it's likely an undocumented RNG: data at +0, control at +4.
pub const RNG_DR: Reg<u32, RW> = reg(0x4000_B800);
pub const RNG_CR: Reg<u32, RW> = reg(0x4000_B804);

/// The information blocks (notes.md "Information blocks").
pub const INFO: usize = 0x1FFF_F000;

#[inline(always)]
pub fn cpsid() {
    unsafe { asm!("cpsid i", options(nomem, nostack, preserves_flags)) }
}

#[inline(always)]
pub fn cpsie() {
    unsafe { asm!("cpsie i", options(nomem, nostack, preserves_flags)) }
}

#[inline(always)]
pub fn dsb_isb() {
    unsafe { asm!("dsb sy", "isb sy", options(nostack, preserves_flags)) }
}
