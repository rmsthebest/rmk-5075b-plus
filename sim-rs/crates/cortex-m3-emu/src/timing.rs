//! Cycle costs: the Cortex-M3 TRM r2p0 (ARM DDI 0337G) Table 18-1 and its
//! notes, section 18.3 (load/store pairing) and sections 5.5-5.6 (exception
//! entry, tail-chaining), for zero wait-state memory. The board agrees: its
//! `asm::delay` loop (SUBS + a taken BNE) takes 3 cycles a round at 96 MHz
//! (BOARD_FACTS.md), so flash fetches from the cache cost no wait states.
//!
//! Where the TRM gives only a range, the pick is noted:
//! - pipeline refill P: 1 after an immediate branch and 2 after a register
//!   one (note a), 3 after a load into the PC (18.3: LDR PC is 2 + 3), one
//!   more to a 32-bit instruction at a halfword address (note a: "longer");
//! - long multiplies: the low end with both operands under 64K (note c),
//!   one cycle more for each operand that isn't;
//! - divides: 2 when the divisor is 0 or larger than the dividend (note e),
//!   otherwise 3-12, rising with the quotient's bit count;
//! - MRS, MSR, CPS (1-2) and IT (0-1, "can be folded") take 1;
//! - exception return: 10, from Figure 5-5 (eight pops and the reload; the
//!   TRM gives no number). Late arrival and a pop abandoned for a new
//!   exception aren't modelled.

use crate::Bus;
use crate::cpu::{Cpu, PC};
use crate::decode::{Addr, Insn, Op, Width, is_wide};

/// From the exception (or the instruction raising it) to the handler's
/// first instruction, stacking included (5.5, Figure 5-2).
pub const ENTRY: u64 = 12;
/// From an exception return straight into the next handler (5.6).
pub const TAIL_CHAIN: u64 = 6;
/// An exception return's unstacking and reload (Figure 5-5).
pub const EXIT: u64 = 10;

/// What an instruction costs before its branch, if it takes one, and the
/// register a pipelined load wrote (for the next load or store, 18.3).
pub(crate) struct Cost {
    pub cycles: u64,
    pub load: Option<u8>,
}

/// Extra cycles for an unaligned access (18.3).
fn unaligned(a: u32, width: Width) -> u64 {
    match width {
        Width::Half if a & 1 != 0 => 1,
        Width::Word if a & 1 != 0 => 2,
        Width::Word if a & 2 != 0 => 1,
        _ => 0,
    }
}

/// Whether the address uses `r`.
fn uses(addr: Addr, r: u8) -> bool {
    match addr {
        Addr::Imm { rn, .. } => rn == r,
        Addr::Reg { rn, rm, .. } => rn == r || rm == r,
        Addr::Literal { .. } => false,
    }
}

/// UMULL/SMULL 3-5, UMLAL 4-6, SMLAL 5-7.
fn mul_long(signed: bool, acc: bool, a: u32, b: u32) -> u64 {
    let big = |v: u32| {
        let m = if signed { (v as i32).unsigned_abs() } else { v };
        (m >= 0x1_0000) as u64
    };
    let n = big(a) + big(b);
    if acc { 4 + signed as u64 + n } else { 3 + n }
}

/// SDIV/UDIV: 2-12.
fn div(signed: bool, n: u32, m: u32) -> u64 {
    let (n, m) = if signed {
        ((n as i32).unsigned_abs(), (m as i32).unsigned_abs())
    } else {
        (n, m)
    };
    if m == 0 || m > n {
        return 2;
    }
    let bits = (m.leading_zeros() - n.leading_zeros()) as u64;
    3 + bits * 9 / 31
}

impl Cpu {
    /// The cost of `insn` executing now, read before it runs (its operands).
    pub(crate) fn cost(&self, insn: &Insn) -> Cost {
        let mut load = None;
        let cycles = match insn.op {
            Op::Mla { .. } | Op::Mls { .. } => 2,
            Op::MulLong {
                signed,
                acc,
                rn,
                rm,
                ..
            } => mul_long(signed, acc, self.reg(rn), self.reg(rm)),
            Op::Div { signed, rn, rm, .. } => div(signed, self.reg(rn), self.reg(rm)),
            Op::Load {
                width, rt, addr, ..
            } => {
                let (a, wb) = self.address(addr);
                let pipelined = wb.is_none() && self.last_load.is_some_and(|r| !uses(addr, r));
                if wb.is_none() && rt != PC as u8 {
                    load = Some(rt);
                }
                (if pipelined { 1 } else { 2 }) + unaligned(a, width)
            }
            Op::Store { width, addr, .. } => {
                let (a, wb) = self.address(addr);
                let base = match addr {
                    Addr::Imm { wback: false, .. } => 1,
                    _ if wb.is_none() && self.last_load.is_some_and(|r| !uses(addr, r)) => 1,
                    _ => 2,
                };
                base + unaligned(a, width)
            }
            Op::LoadDual { .. } | Op::StoreDual { .. } => 3,
            Op::Ldm { regs, .. } | Op::Stm { regs, .. } => 1 + regs.count_ones() as u64,
            Op::Ldrex { .. } | Op::Strex { .. } => 2,
            // 2 for the load and 1 for the add (18.3), then the reload.
            Op::Tb { .. } => 3,
            _ => 1,
        };
        Cost { cycles, load }
    }

    /// The pipeline refill after `insn` wrote the PC (not an exception
    /// return).
    pub(crate) fn refill(&self, bus: &mut impl Bus, insn: &Insn) -> u64 {
        let p = match insn.op {
            Op::B { .. } | Op::Bl { .. } | Op::Cbz { .. } => 1,
            Op::Load { .. } | Op::Ldm { .. } | Op::Tb { .. } => 3,
            _ => 2,
        };
        let target = self.r[PC];
        let split = target & 2 != 0 && bus.fetch(target).is_some_and(is_wide);
        p + split as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_multiply_and_divide_stay_in_the_trms_ranges() {
        assert_eq!(mul_long(false, false, 3, 0xFFFF), 3);
        assert_eq!(mul_long(false, false, 0x1_0000, 0xFFFF_FFFF), 5);
        assert_eq!(mul_long(true, false, -5i32 as u32, 7), 3);
        assert_eq!(mul_long(false, true, 1, 1), 4);
        assert_eq!(mul_long(true, true, i32::MIN as u32, i32::MIN as u32), 7);
        assert_eq!(div(false, 5, 0), 2);
        assert_eq!(div(false, 5, 6), 2);
        assert_eq!(div(false, 7, 5), 3);
        assert_eq!(div(false, u32::MAX, 1), 12);
        assert_eq!(div(true, -100i32 as u32, 7), div(false, 100, 7));
    }
}
