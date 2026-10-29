//! Printing decoded instructions in LLVM's syntax (llvm-objdump), so the
//! decoder can be checked against it and traces read like objdump output.
//! Width suffixes (`.w`) aren't printed: they don't change meaning.

use crate::decode::*;
use std::fmt::Write;

pub fn reg(r: u8) -> &'static str {
    [
        "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp",
        "lr", "pc",
    ][r as usize & 15]
}

pub fn cond_name(c: Cond) -> &'static str {
    use Cond::*;
    match c {
        Eq => "eq",
        Ne => "ne",
        Hs => "hs",
        Lo => "lo",
        Mi => "mi",
        Pl => "pl",
        Vs => "vs",
        Vc => "vc",
        Hi => "hi",
        Ls => "ls",
        Ge => "ge",
        Lt => "lt",
        Gt => "gt",
        Le => "le",
        Al => "",
    }
}

pub fn sysreg_name(sysm: u8) -> Option<&'static str> {
    Some(match sysm {
        0 => "apsr",
        1 => "iapsr",
        2 => "eapsr",
        3 => "xpsr",
        5 => "ipsr",
        6 => "epsr",
        7 => "iepsr",
        8 => "msp",
        9 => "psp",
        16 => "primask",
        17 => "basepri",
        18 => "basepri_max",
        19 => "faultmask",
        20 => "control",
        _ => return None,
    })
}

fn hex(v: i64) -> String {
    if v < 0 {
        format!("#-0x{:x}", -v)
    } else {
        format!("#0x{v:x}")
    }
}

fn shift_suffix(s: Shift) -> String {
    let name = match s.kind {
        ShiftKind::Lsl if s.amount == 0 => return String::new(),
        ShiftKind::Rrx => return ", rrx".into(),
        ShiftKind::Lsl => "lsl",
        ShiftKind::Lsr => "lsr",
        ShiftKind::Asr => "asr",
        ShiftKind::Ror => "ror",
    };
    format!(", {name} #{}", s.amount)
}

fn operand(op2: Operand) -> String {
    match op2 {
        Operand::Imm { value, .. } => hex(value as i64),
        Operand::Reg { rm, shift } => format!("{}{}", reg(rm), shift_suffix(shift)),
    }
}

fn reglist(regs: u16) -> String {
    let names: Vec<_> = (0..16)
        .filter(|r| regs & (1 << r) != 0)
        .map(|r| reg(r as u8))
        .collect();
    format!("{{{}}}", names.join(", "))
}

fn address(a: Addr) -> String {
    match a {
        Addr::Imm {
            rn,
            off: 0,
            index: true,
            wback: false,
        } => format!("[{}]", reg(rn)),
        Addr::Imm {
            rn,
            off,
            index: true,
            wback: false,
        } => format!("[{}, {}]", reg(rn), hex(off as i64)),
        Addr::Imm {
            rn,
            off,
            index: true,
            wback: true,
        } => format!("[{}, {}]!", reg(rn), hex(off as i64)),
        Addr::Imm { rn, off, .. } => format!("[{}], {}", reg(rn), hex(off as i64)),
        Addr::Reg { rn, rm, shift: 0 } => format!("[{}, {}]", reg(rn), reg(rm)),
        Addr::Reg { rn, rm, shift } => format!("[{}, {}, lsl #{shift}]", reg(rn), reg(rm)),
        Addr::Literal { off } => format!("[pc, {}]", hex(off as i64)),
    }
}

fn width_suffix(width: Width, signed: bool) -> &'static str {
    match (width, signed) {
        (Width::Word, _) => "",
        (Width::Byte, false) => "b",
        (Width::Half, false) => "h",
        (Width::Byte, true) => "sb",
        (Width::Half, true) => "sh",
    }
}

fn dp_name(op: DpOp) -> &'static str {
    use DpOp::*;
    match op {
        And => "and",
        Eor => "eor",
        Orr => "orr",
        Orn => "orn",
        Bic => "bic",
        Add => "add",
        Adc => "adc",
        Sbc => "sbc",
        Sub => "sub",
        Rsb => "rsb",
        Mov => "mov",
        Mvn => "mvn",
        Tst => "tst",
        Teq => "teq",
        Cmp => "cmp",
        Cmn => "cmn",
    }
}

fn shift_name(k: ShiftKind) -> &'static str {
    match k {
        ShiftKind::Lsl => "lsl",
        ShiftKind::Lsr => "lsr",
        ShiftKind::Asr => "asr",
        ShiftKind::Ror => "ror",
        ShiftKind::Rrx => "rrx",
    }
}

/// The IT mnemonic suffix: one t/e per instruction after the first.
pub fn it_suffix(firstcond: u8, mask: u8) -> String {
    // 0b1111 is UNPREDICTABLE; LLVM reads it as AL (0b1110).
    let firstcond = firstcond.min(14);
    let mut s = String::new();
    let n = 3 - mask.trailing_zeros().min(3);
    for i in 0..n {
        let b = (mask >> (3 - i)) & 1;
        s.push(if b == firstcond & 1 { 't' } else { 'e' });
    }
    s
}

/// `addr` is where the instruction is (for branch targets); `cond` the IT
/// block's condition on it, if it's in one.
pub fn disasm(insn: &Insn, addr: u32, cond: Option<Cond>) -> String {
    let c = cond.map(cond_name).unwrap_or("");
    let s_ = |s: bool| if s { "s" } else { "" };
    let pc = addr.wrapping_add(4);
    let target = |off: i32| format!("0x{:x}", pc.wrapping_add(off as u32));
    let mut out = String::new();
    let o = &mut out;
    match insn.op {
        Op::Dp {
            op,
            s,
            rd,
            rn,
            op2,
            form,
        } => match (op, form) {
            (DpOp::Mov, _) => match op2 {
                Operand::Reg { rm, shift } if shift.kind == ShiftKind::Rrx => {
                    let _ = write!(o, "rrx{}{c} {}, {}", s_(s), reg(rd), reg(rm));
                }
                Operand::Reg { rm, shift } if !shift.is_none() => {
                    let _ = write!(
                        o,
                        "{}{}{c} {}, {}, #0x{:x}",
                        shift_name(shift.kind),
                        s_(s),
                        reg(rd),
                        reg(rm),
                        shift.amount
                    );
                }
                // MOVS T2 (LSL #0) isn't allowed in an IT block; LLVM
                // prints it without the block's condition.
                Operand::Reg { .. } if s && insn.len == 2 => {
                    let _ = write!(o, "movs {}, {}", reg(rd), operand(op2));
                }
                _ => {
                    let _ = write!(o, "mov{}{c} {}, {}", s_(s), reg(rd), operand(op2));
                }
            },
            (DpOp::Mvn, _) => {
                let _ = write!(o, "mvn{}{c} {}, {}", s_(s), reg(rd), operand(op2));
            }
            (op, _) if op.is_test() => {
                let _ = write!(o, "{}{c} {}, {}", dp_name(op), reg(rn), operand(op2));
            }
            (op, DpForm::Adr) => {
                let Operand::Imm { value, .. } = op2 else {
                    unreachable!()
                };
                let v = if op == DpOp::Sub {
                    -(value as i64)
                } else {
                    value as i64
                };
                let _ = write!(o, "adr{c} {}, {}", reg(rd), hex(v));
            }
            (op, DpForm::Wide) => {
                let _ = write!(
                    o,
                    "{}w{c} {}, {}, {}",
                    dp_name(op),
                    reg(rd),
                    reg(rn),
                    operand(op2)
                );
            }
            (op, DpForm::Two) => {
                let _ = write!(
                    o,
                    "{}{}{c} {}, {}",
                    dp_name(op),
                    s_(s),
                    reg(rd),
                    operand(op2)
                );
            }
            (op, DpForm::Three) => {
                let _ = write!(
                    o,
                    "{}{}{c} {}, {}, {}",
                    dp_name(op),
                    s_(s),
                    reg(rd),
                    reg(rn),
                    operand(op2)
                );
            }
        },
        Op::ShiftReg {
            kind,
            s,
            rd,
            rn,
            rm,
            two,
        } => {
            let _ = write!(o, "{}{}{c} {}, ", shift_name(kind), s_(s), reg(rd));
            if !two {
                let _ = write!(o, "{}, ", reg(rn));
            }
            o.push_str(reg(rm));
        }
        Op::Movw { rd, imm } => {
            let _ = write!(o, "movw{c} {}, {}", reg(rd), hex(imm as i64));
        }
        Op::Movt { rd, imm } => {
            let _ = write!(o, "movt{c} {}, {}", reg(rd), hex(imm as i64));
        }
        Op::Mul { s, rd, rn, rm } => {
            let _ = write!(o, "mul{}{c} {}, {}, {}", s_(s), reg(rd), reg(rn), reg(rm));
        }
        Op::Mla { rd, rn, rm, ra } => {
            let _ = write!(
                o,
                "mla{c} {}, {}, {}, {}",
                reg(rd),
                reg(rn),
                reg(rm),
                reg(ra)
            );
        }
        Op::Mls { rd, rn, rm, ra } => {
            let _ = write!(
                o,
                "mls{c} {}, {}, {}, {}",
                reg(rd),
                reg(rn),
                reg(rm),
                reg(ra)
            );
        }
        Op::MulLong {
            signed,
            acc,
            rdlo,
            rdhi,
            rn,
            rm,
        } => {
            let name = match (signed, acc) {
                (false, false) => "umull",
                (true, false) => "smull",
                (false, true) => "umlal",
                (true, true) => "smlal",
            };
            let _ = write!(
                o,
                "{name}{c} {}, {}, {}, {}",
                reg(rdlo),
                reg(rdhi),
                reg(rn),
                reg(rm)
            );
        }
        Op::Div { signed, rd, rn, rm } => {
            let name = if signed { "sdiv" } else { "udiv" };
            let _ = write!(o, "{name}{c} {}, {}, {}", reg(rd), reg(rn), reg(rm));
        }
        Op::Extend { kind, rd, rm, rot } => {
            let name = match kind {
                ExtendKind::Sxtb => "sxtb",
                ExtendKind::Sxth => "sxth",
                ExtendKind::Uxtb => "uxtb",
                ExtendKind::Uxth => "uxth",
            };
            let _ = write!(o, "{name}{c} {}, {}", reg(rd), reg(rm));
            if rot != 0 {
                let _ = write!(o, ", ror #{rot}");
            }
        }
        Op::Clz { rd, rm } => {
            let _ = write!(o, "clz{c} {}, {}", reg(rd), reg(rm));
        }
        Op::Rev { rd, rm } => {
            let _ = write!(o, "rev{c} {}, {}", reg(rd), reg(rm));
        }
        Op::Rev16 { rd, rm } => {
            let _ = write!(o, "rev16{c} {}, {}", reg(rd), reg(rm));
        }
        Op::Revsh { rd, rm } => {
            let _ = write!(o, "revsh{c} {}, {}", reg(rd), reg(rm));
        }
        Op::Rbit { rd, rm } => {
            let _ = write!(o, "rbit{c} {}, {}", reg(rd), reg(rm));
        }
        Op::Bfi { rd, rn, lsb, width } => {
            let _ = write!(o, "bfi{c} {}, {}, #{lsb}, #{width}", reg(rd), reg(rn));
        }
        Op::Bfc { rd, lsb, width } => {
            let _ = write!(o, "bfc{c} {}, #{lsb}, #{width}", reg(rd));
        }
        Op::Bfx {
            signed,
            rd,
            rn,
            lsb,
            width,
        } => {
            let name = if signed { "sbfx" } else { "ubfx" };
            let _ = write!(o, "{name}{c} {}, {}, #{lsb}, #{width}", reg(rd), reg(rn));
        }
        Op::Sat {
            signed,
            rd,
            sat,
            rn,
            shift,
        } => {
            let name = if signed { "ssat" } else { "usat" };
            let _ = write!(
                o,
                "{name}{c} {}, #{sat}, {}{}",
                reg(rd),
                reg(rn),
                shift_suffix(shift)
            );
        }
        Op::Load {
            width,
            signed,
            rt,
            addr,
            unpriv,
        } => {
            let t = if unpriv { "t" } else { "" };
            let _ = write!(
                o,
                "ldr{}{t}{c} {}, {}",
                width_suffix(width, signed),
                reg(rt),
                address(addr)
            );
        }
        Op::Store {
            width,
            rt,
            addr,
            unpriv,
        } => {
            let t = if unpriv { "t" } else { "" };
            let _ = write!(
                o,
                "str{}{t}{c} {}, {}",
                width_suffix(width, false),
                reg(rt),
                address(addr)
            );
        }
        Op::LoadDual { rt, rt2, addr } => {
            let _ = write!(o, "ldrd{c} {}, {}, {}", reg(rt), reg(rt2), address(addr));
        }
        Op::StoreDual { rt, rt2, addr } => {
            let _ = write!(o, "strd{c} {}, {}, {}", reg(rt), reg(rt2), address(addr));
        }
        Op::Ldm {
            rn: 13,
            regs,
            wback: true,
            db: false,
        } => {
            let _ = write!(o, "pop{c} {}", reglist(regs));
        }
        Op::Stm {
            rn: 13,
            regs,
            wback: true,
            db: true,
        } => {
            let _ = write!(o, "push{c} {}", reglist(regs));
        }
        Op::Ldm {
            rn,
            regs,
            wback,
            db,
        }
        | Op::Stm {
            rn,
            regs,
            wback,
            db,
        } => {
            let name = if matches!(insn.op, Op::Ldm { .. }) {
                "ldm"
            } else {
                "stm"
            };
            let db = if db { "db" } else { "" };
            let w = if wback { "!" } else { "" };
            let _ = write!(o, "{name}{db}{c} {}{w}, {}", reg(rn), reglist(regs));
        }
        Op::Ldrex { width, rt, rn, off } => {
            let _ = write!(o, "ldrex{}{c} {}, ", width_suffix(width, false), reg(rt));
            o.push_str(&address(Addr::Imm {
                rn,
                off: off as i32,
                index: true,
                wback: false,
            }));
        }
        Op::Strex {
            width,
            rd,
            rt,
            rn,
            off,
        } => {
            let _ = write!(
                o,
                "strex{}{c} {}, {}, ",
                width_suffix(width, false),
                reg(rd),
                reg(rt)
            );
            o.push_str(&address(Addr::Imm {
                rn,
                off: off as i32,
                index: true,
                wback: false,
            }));
        }
        Op::Clrex => {
            let _ = write!(o, "clrex{c}");
        }
        Op::B { cond: bc, off } => {
            let bc = if bc == Cond::Al { c } else { cond_name(bc) };
            let _ = write!(o, "b{bc} {}", target(off));
        }
        Op::Bl { off } => {
            let _ = write!(o, "bl{c} {}", target(off));
        }
        Op::Bx { rm } => {
            let _ = write!(o, "bx{c} {}", reg(rm));
        }
        Op::Blx { rm } => {
            let _ = write!(o, "blx{c} {}", reg(rm));
        }
        Op::Cbz { nz, rn, off } => {
            let name = if nz { "cbnz" } else { "cbz" };
            let _ = write!(o, "{name} {}, {}", reg(rn), target(off as i32));
        }
        Op::Tb {
            half: false,
            rn,
            rm,
        } => {
            let _ = write!(o, "tbb{c} [{}, {}]", reg(rn), reg(rm));
        }
        Op::Tb { half: true, rn, rm } => {
            let _ = write!(o, "tbh{c} [{}, {}, lsl #1]", reg(rn), reg(rm));
        }
        Op::It { firstcond, mask } => {
            let first = match Cond::from_bits(firstcond as u32) {
                Cond::Al => "al",
                fc => cond_name(fc),
            };
            let _ = write!(o, "it{} {first}", it_suffix(firstcond, mask));
        }
        Op::Hint(h) => {
            let name = match h {
                Hint::Nop => "nop",
                Hint::Yield => "yield",
                Hint::Wfe => "wfe",
                Hint::Wfi => "wfi",
                Hint::Sev => "sev",
                Hint::Other(n) => {
                    let _ = write!(o, "hint{c} #{n}");
                    return out;
                }
            };
            let _ = write!(o, "{name}{c}");
        }
        Op::Barrier { kind, opt } => {
            let name = match kind {
                Barrier::Dsb => "dsb",
                Barrier::Dmb => "dmb",
                Barrier::Isb => "isb",
            };
            let opt = match opt {
                15 => "sy".to_string(),
                14 => "st".into(),
                11 => "ish".into(),
                10 => "ishst".into(),
                7 => "nsh".into(),
                6 => "nshst".into(),
                3 => "osh".into(),
                2 => "oshst".into(),
                n => format!("#0x{n:x}"),
            };
            let _ = write!(o, "{name}{c} {opt}");
        }
        Op::Mrs { rd, sysm } => {
            let name = sysreg_name(sysm)
                .map(String::from)
                .unwrap_or_else(|| sysm.to_string());
            let _ = write!(o, "mrs{c} {}, {name}", reg(rd));
        }
        Op::Msr { rn, sysm, mask } => {
            let mut name = sysreg_name(sysm)
                .map(String::from)
                .unwrap_or_else(|| sysm.to_string());
            if sysm < 4 {
                name.push_str(match mask {
                    1 => "_g",
                    2 => "_nzcvq",
                    3 => "_nzcvqg",
                    _ => "",
                });
            }
            let _ = write!(o, "msr{c} {name}, {}", reg(rn));
        }
        Op::Cps { disable, a, i, f } => {
            let name = if disable { "cpsid" } else { "cpsie" };
            let mut flags: String = [(a, 'a'), (i, 'i'), (f, 'f')]
                .iter()
                .filter(|x| x.0)
                .map(|x| x.1)
                .collect();
            if flags.is_empty() {
                flags.push_str("none");
            }
            let _ = write!(o, "{name} {flags}");
        }
        Op::Preload { instr, addr } => {
            let name = if instr { "pli" } else { "pld" };
            let _ = write!(o, "{name}{c} {}", address(addr));
        }
        Op::Svc { imm } => {
            let _ = write!(o, "svc{c} {}", hex(imm as i64));
        }
        Op::Bkpt { imm } => {
            let _ = write!(o, "bkpt {}", hex(imm as i64));
        }
        // LLVM's alias for the 16-bit UDF #254 (`__builtin_trap`).
        Op::Udf { imm: 0xFE } if insn.len == 2 => o.push_str("trap"),
        // And the one for UDF #249 (`__brkdiv0`, divide by zero on Windows).
        Op::Udf { imm: 0xF9 } if insn.len == 2 => o.push_str("__brkdiv0"),
        Op::Udf { imm } => {
            let _ = write!(o, "udf {}", hex(imm as i64));
        }
        Op::Undefined => o.push_str("<unknown>"),
    }
    out
}
