//! The flash controller. ROM 0x1FFFE104-0x1FFFE177 (one operation) and
//! 0x1FFFEE54-0x1FFFEF37 (the preparation before each). QMK's EFL driver
//! carries the same two pieces: `pre_op_code` is 0x1FFFEE54 byte for byte,
//! and `FLASH_OP_RAM_CODE` is RAM_CODE below.

use crate::mmio::*;
use core::arch::asm;
use wb32_pac::{ANCTL, FMC, PWR, RCC};

// FMC_CON operations (the low 5 bits).
/// Clears the page latch (FMC+0x100) before it's filled.
pub const CLEAR_LATCH: u32 = 0x04;
/// Erases the 256-byte page at FMC_ADDR.
pub const ERASE_PAGE: u32 = 0x08;
/// Programs the latch into the page at FMC_ADDR.
pub const PROGRAM: u32 = 0x0C;
/// Erases all of main flash.
pub const CHIP_ERASE: u32 = 0x1B;

/// ROM 0x1FFFEF38: `str r0, [r1]; nop; 1: ldr r0, [r1]; lsls r0, r0, #24;
/// bmi 1b; bx lr`. Starts the operation and waits for FMC_CON bit 7 to
/// clear. The ROM copies it to the stack and runs it from there, as code
/// in flash would have to; the ROM itself wouldn't need to.
const RAM_CODE: [u16; 6] = [0x6008, 0xBF00, 0x6808, 0x0600, 0xD4FC, 0x4770];

/// ROM 0x1FFFE104-0x1FFFE15F: one operation on `addr`. True if FMC_STAT
/// reports an error (bit 2).
pub fn op(addr: u32, op: u32) -> bool {
    prepare();
    w32(FMC.addr(), addr);
    let code = RAM_CODE;
    w32(RCC.pclkenr(), 1);
    w32(FMC.con(), (op & 0x1F) | 0x7F5F_0D40);
    w32(FMC.key(), 0x5188_DA08);
    w32(FMC.key(), 0x1258_6590);
    // Start (0x800080), run from the stack.
    unsafe {
        asm!(
            "blx {code}",
            code = in(reg) code.as_ptr() as usize | 1,
            in("r0") 0x0080_0080u32,
            in("r1") FMC.con().as_ptr(),
            lateout("r0") _,
            clobber_abi("C"),
        );
    }
    w32(RCC.pclkenr(), 0);
    w32(FMC.con(), 0x005F_0000);
    r32(FMC.stat()) & 0x04 != 0
}

/// ROM 0x1FFFEE54-0x1FFFEF37: restarts FHSI, then checks it through the
/// RNG at 0x4000B800 (mmio::RNG_RAND): of ten samples, more than two equal
/// to the one before means it's stuck, and FHSI is retrimmed from
/// information block 3 (0x1FFFF340 bits 20-27, less 16 per try) and
/// restarted. Up to five tries; the analog registers are left unlocked.
fn prepare() {
    w32(PWR.anakey1(), 0x03);
    w32(PWR.anakey2(), 0x0C);
    w32(ANCTL.fhsienr(), 0);
    spin(1000);
    w32(ANCTL.fhsienr(), 1);
    spin(1000);
    m32(RCC.apb2pre(), |v| v | 0x80);
    m32(RCC.apb2enr(), |v| v | 0xA00);
    w32(RCC.rngclkenr(), 1);
    w32(RNG_STOP, 0);
    let mut trim = (r32(reg(INFO + 0x340)) >> 20) & 0xFF;
    for _ in 0..5 {
        let mut same = 0;
        let mut prev = 0xFFFF_FFFF;
        for i in 0..20 {
            let v = r32(RNG_RAND);
            if i & 1 == 0 {
                same += (v == prev) as u32;
                prev = v;
            }
        }
        if same <= 2 {
            return;
        }
        // 16 lower on every try.
        trim = if trim > 16 { trim - 16 } else { 0 };
        m32(ANCTL_FHSITRIM, |v| (v & !0x0FF0_0000) | trim << 20);
        w32(ANCTL.fhsienr(), 0);
        spin(1000);
        w32(ANCTL.fhsienr(), 1);
        spin(1000);
    }
}

/// The ROM's `for (i = 0; i < n; i++);`.
fn spin(n: u32) {
    for _ in 0..n {
        unsafe { asm!("nop", options(nomem, nostack, preserves_flags)) }
    }
}
