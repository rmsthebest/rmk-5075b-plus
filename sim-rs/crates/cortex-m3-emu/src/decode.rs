//! Thumb-2 decoding for ARMv7-M (ARM DDI 0403E, chapter A5), into [`Op`]s
//! the interpreter executes and `disasm` prints.
//!
//! Decoding depends on whether the instruction sits in an IT block: there,
//! the 16-bit data-processing forms don't set flags.

/// Condition codes, in encoding order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Hs,
    Lo,
    Mi,
    Pl,
    Vs,
    Vc,
    Hi,
    Ls,
    Ge,
    Lt,
    Gt,
    Le,
    Al,
}

impl Cond {
    pub fn from_bits(bits: u32) -> Cond {
        use Cond::*;
        [
            Eq, Ne, Hs, Lo, Mi, Pl, Vs, Vc, Hi, Ls, Ge, Lt, Gt, Le, Al, Al,
        ][bits as usize & 15]
    }

    pub fn bits(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftKind {
    Lsl,
    Lsr,
    Asr,
    Ror,
    /// Rotate right by one through carry; the amount is 1.
    Rrx,
}

/// A shift by a constant, as DecodeImmShift gives it: amounts are 0..=32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shift {
    pub kind: ShiftKind,
    pub amount: u8,
}

impl Shift {
    pub const NONE: Shift = Shift {
        kind: ShiftKind::Lsl,
        amount: 0,
    };

    /// DecodeImmShift.
    pub fn decode(ty: u32, imm5: u32) -> Shift {
        let amount = imm5 as u8;
        match ty & 3 {
            0 => Shift {
                kind: ShiftKind::Lsl,
                amount,
            },
            1 => Shift {
                kind: ShiftKind::Lsr,
                amount: if amount == 0 { 32 } else { amount },
            },
            2 => Shift {
                kind: ShiftKind::Asr,
                amount: if amount == 0 { 32 } else { amount },
            },
            _ if amount == 0 => Shift {
                kind: ShiftKind::Rrx,
                amount: 1,
            },
            _ => Shift {
                kind: ShiftKind::Ror,
                amount,
            },
        }
    }

    pub fn is_none(self) -> bool {
        self.kind == ShiftKind::Lsl && self.amount == 0
    }
}

/// The flexible second operand of data processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// `carry` is the carry out of ThumbExpandImm_C, if it changes C.
    Imm {
        value: u32,
        carry: Option<bool>,
    },
    Reg {
        rm: u8,
        shift: Shift,
    },
}

impl Operand {
    pub const fn imm(value: u32) -> Operand {
        Operand::Imm { value, carry: None }
    }

    pub const fn reg(rm: u8) -> Operand {
        Operand::Reg {
            rm,
            shift: Shift::NONE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpOp {
    And,
    Eor,
    Orr,
    Orn,
    Bic,
    Add,
    Adc,
    Sbc,
    Sub,
    Rsb,
    Mov,
    Mvn,
    Tst,
    Teq,
    Cmp,
    Cmn,
}

impl DpOp {
    /// Compares and tests: no destination, always set flags.
    pub fn is_test(self) -> bool {
        matches!(self, DpOp::Tst | DpOp::Teq | DpOp::Cmp | DpOp::Cmn)
    }
}

/// How a data-processing instruction prints (it doesn't change what it does).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpForm {
    /// `op rd, rn, op2`
    Three,
    /// The 16-bit forms where rd is rn: `op rd, op2`
    Two,
    /// ADDW/SUBW: a plain 12-bit immediate
    Wide,
    /// ADR: rn is PC, word-aligned
    Adr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Byte,
    Half,
    Word,
}

impl Width {
    pub fn bytes(self) -> u32 {
        match self {
            Width::Byte => 1,
            Width::Half => 2,
            Width::Word => 4,
        }
    }
}

/// Load/store addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr {
    /// `[rn, #off]`, `[rn, #off]!` (index and wback), `[rn], #off` (wback only)
    Imm {
        rn: u8,
        off: i32,
        index: bool,
        wback: bool,
    },
    /// `[rn, rm, lsl #shift]`
    Reg { rn: u8, rm: u8, shift: u8 },
    /// `[pc, #off]` from Align(PC, 4)
    Literal { off: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendKind {
    Sxtb,
    Sxth,
    Uxtb,
    Uxth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Nop,
    Yield,
    Wfe,
    Wfi,
    Sev,
    /// Any other hint number: no effect.
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    Dsb,
    Dmb,
    Isb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Dp {
        op: DpOp,
        s: bool,
        rd: u8,
        rn: u8,
        op2: Operand,
        form: DpForm,
    },
    /// LSL/LSR/ASR/ROR by a register.
    ShiftReg {
        kind: ShiftKind,
        s: bool,
        rd: u8,
        rn: u8,
        rm: u8,
        two: bool,
    },
    Movw {
        rd: u8,
        imm: u16,
    },
    Movt {
        rd: u8,
        imm: u16,
    },
    Mul {
        s: bool,
        rd: u8,
        rn: u8,
        rm: u8,
    },
    Mla {
        rd: u8,
        rn: u8,
        rm: u8,
        ra: u8,
    },
    Mls {
        rd: u8,
        rn: u8,
        rm: u8,
        ra: u8,
    },
    /// UMULL/SMULL/UMLAL/SMLAL
    MulLong {
        signed: bool,
        acc: bool,
        rdlo: u8,
        rdhi: u8,
        rn: u8,
        rm: u8,
    },
    Div {
        signed: bool,
        rd: u8,
        rn: u8,
        rm: u8,
    },
    Extend {
        kind: ExtendKind,
        rd: u8,
        rm: u8,
        rot: u8,
    },
    Clz {
        rd: u8,
        rm: u8,
    },
    Rev {
        rd: u8,
        rm: u8,
    },
    Rev16 {
        rd: u8,
        rm: u8,
    },
    Revsh {
        rd: u8,
        rm: u8,
    },
    Rbit {
        rd: u8,
        rm: u8,
    },
    Bfi {
        rd: u8,
        rn: u8,
        lsb: u8,
        width: u8,
    },
    Bfc {
        rd: u8,
        lsb: u8,
        width: u8,
    },
    Bfx {
        signed: bool,
        rd: u8,
        rn: u8,
        lsb: u8,
        width: u8,
    },
    /// SSAT/USAT: `sat` is the printed bit position (1..=32 signed, 0..=31 unsigned).
    Sat {
        signed: bool,
        rd: u8,
        sat: u8,
        rn: u8,
        shift: Shift,
    },

    Load {
        width: Width,
        signed: bool,
        rt: u8,
        addr: Addr,
        unpriv: bool,
    },
    Store {
        width: Width,
        rt: u8,
        addr: Addr,
        unpriv: bool,
    },
    LoadDual {
        rt: u8,
        rt2: u8,
        addr: Addr,
    },
    StoreDual {
        rt: u8,
        rt2: u8,
        addr: Addr,
    },
    /// LDM (increment after) / LDMDB; POP is LDM SP!.
    Ldm {
        rn: u8,
        regs: u16,
        wback: bool,
        db: bool,
    },
    Stm {
        rn: u8,
        regs: u16,
        wback: bool,
        db: bool,
    },
    Ldrex {
        width: Width,
        rt: u8,
        rn: u8,
        off: u16,
    },
    Strex {
        width: Width,
        rd: u8,
        rt: u8,
        rn: u8,
        off: u16,
    },
    Clrex,

    /// B with its own condition (Al outside of the conditional encodings).
    B {
        cond: Cond,
        off: i32,
    },
    Bl {
        off: i32,
    },
    Bx {
        rm: u8,
    },
    Blx {
        rm: u8,
    },
    Cbz {
        nz: bool,
        rn: u8,
        off: u8,
    },
    Tb {
        half: bool,
        rn: u8,
        rm: u8,
    },

    It {
        firstcond: u8,
        mask: u8,
    },
    Hint(Hint),
    /// PLD (`instr` false) / PLI: no effect.
    Preload {
        instr: bool,
        addr: Addr,
    },
    Barrier {
        kind: Barrier,
        opt: u8,
    },
    Mrs {
        rd: u8,
        sysm: u8,
    },
    Msr {
        rn: u8,
        sysm: u8,
        mask: u8,
    },
    /// `a` is ARMv7-A's abort mask bit, (0) here: printed, ignored.
    Cps {
        disable: bool,
        a: bool,
        i: bool,
        f: bool,
    },
    Svc {
        imm: u8,
    },
    Bkpt {
        imm: u8,
    },
    Udf {
        imm: u16,
    },
    /// Not an ARMv7-M instruction (or one this core lacks, such as a
    /// coprocessor op): executing it is a UsageFault.
    Undefined,
}

/// A decoded instruction and its length in bytes (2 or 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub op: Op,
    pub len: u8,
    /// The encoding breaks a should-be bit, or a rule the ARM ARM calls
    /// UNPREDICTABLE (an empty register list, a reserved SYSm). `op` is
    /// still what the fields say; executing it is reported as an error.
    pub unpredictable: bool,
}

/// Whether `hw1` starts a 32-bit instruction.
pub fn is_wide(hw1: u16) -> bool {
    hw1 >> 11 >= 0b11101
}

fn bit(v: u32, n: u32) -> bool {
    (v >> n) & 1 != 0
}

fn bits(v: u32, hi: u32, lo: u32) -> u32 {
    (v >> lo) & ((1 << (hi - lo + 1)) - 1)
}

fn sign_extend(v: u32, width: u32) -> i32 {
    ((v << (32 - width)) as i32) >> (32 - width)
}

/// ThumbExpandImm_C, with the carry out only for the rotated forms.
pub fn expand_imm(imm12: u32) -> Operand {
    if imm12 >> 10 == 0 {
        let b = imm12 & 0xFF;
        let value = match (imm12 >> 8) & 3 {
            0 => b,
            1 => b << 16 | b,
            2 => b << 24 | b << 8,
            _ => b << 24 | b << 16 | b << 8 | b,
        };
        Operand::Imm { value, carry: None }
    } else {
        let value = (0x80 | (imm12 & 0x7F)).rotate_right(imm12 >> 7);
        Operand::Imm {
            value,
            carry: Some(value >> 31 != 0),
        }
    }
}

/// Decodes the instruction starting with `hw1` (`hw2` is only looked at for
/// 32-bit ones).
pub fn decode(hw1: u16, hw2: u16, in_it: bool) -> Insn {
    let mut unpredictable = false;
    let u = &mut unpredictable;
    let (op, len) = if is_wide(hw1) {
        (decode32(hw1 as u32, hw2 as u32, u), 4)
    } else {
        (decode16(hw1 as u32, in_it, u), 2)
    };
    Insn {
        op,
        len,
        unpredictable,
    }
}

/// Marks the instruction UNPREDICTABLE when `cond` holds.
fn flag(u: &mut bool, cond: bool) {
    *u |= cond;
}

fn decode16(h: u32, in_it: bool, u: &mut bool) -> Op {
    let s = !in_it;
    let lo3 = |n| bits(h, n + 2, n) as u8;
    match h >> 10 {
        // Shift (immediate), add, subtract, move, compare
        0b000000..=0b001111 => {
            let rd = lo3(0);
            let rn = lo3(3);
            match bits(h, 13, 11) {
                0b000..=0b010 => {
                    let ty = bits(h, 12, 11);
                    let shift = Shift::decode(ty, bits(h, 10, 6));
                    // LSL #0 is MOVS (register), flag-setting even in IT.
                    let s = if shift.is_none() { true } else { s };
                    Op::Dp {
                        op: DpOp::Mov,
                        s,
                        rd,
                        rn: 0,
                        op2: Operand::Reg { rm: rn, shift },
                        form: DpForm::Three,
                    }
                }
                0b011 => {
                    let op = if bit(h, 9) { DpOp::Sub } else { DpOp::Add };
                    let op2 = if bit(h, 10) {
                        Operand::imm(bits(h, 8, 6))
                    } else {
                        Operand::reg(lo3(6))
                    };
                    Op::Dp {
                        op,
                        s,
                        rd,
                        rn,
                        op2,
                        form: DpForm::Three,
                    }
                }
                o => {
                    let rdn = lo3(8);
                    let op2 = Operand::imm(h & 0xFF);
                    match o {
                        0b100 => Op::Dp {
                            op: DpOp::Mov,
                            s,
                            rd: rdn,
                            rn: 0,
                            op2,
                            form: DpForm::Three,
                        },
                        0b101 => Op::Dp {
                            op: DpOp::Cmp,
                            s: true,
                            rd: 0,
                            rn: rdn,
                            op2,
                            form: DpForm::Three,
                        },
                        0b110 => Op::Dp {
                            op: DpOp::Add,
                            s,
                            rd: rdn,
                            rn: rdn,
                            op2,
                            form: DpForm::Two,
                        },
                        _ => Op::Dp {
                            op: DpOp::Sub,
                            s,
                            rd: rdn,
                            rn: rdn,
                            op2,
                            form: DpForm::Two,
                        },
                    }
                }
            }
        }
        // Data processing
        0b010000 => {
            let rdn = lo3(0);
            let rm = lo3(3);
            let dp = |op, s| Op::Dp {
                op,
                s,
                rd: rdn,
                rn: rdn,
                op2: Operand::reg(rm),
                form: DpForm::Two,
            };
            let sh = |kind| Op::ShiftReg {
                kind,
                s,
                rd: rdn,
                rn: rdn,
                rm,
                two: true,
            };
            match bits(h, 9, 6) {
                0b0000 => dp(DpOp::And, s),
                0b0001 => dp(DpOp::Eor, s),
                0b0010 => sh(ShiftKind::Lsl),
                0b0011 => sh(ShiftKind::Lsr),
                0b0100 => sh(ShiftKind::Asr),
                0b0101 => dp(DpOp::Adc, s),
                0b0110 => dp(DpOp::Sbc, s),
                0b0111 => sh(ShiftKind::Ror),
                0b1000 => Op::Dp {
                    op: DpOp::Tst,
                    s: true,
                    rd: 0,
                    rn: rdn,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
                0b1001 => Op::Dp {
                    op: DpOp::Rsb,
                    s,
                    rd: rdn,
                    rn: rm,
                    op2: Operand::imm(0),
                    form: DpForm::Three,
                },
                0b1010 => Op::Dp {
                    op: DpOp::Cmp,
                    s: true,
                    rd: 0,
                    rn: rdn,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
                0b1011 => Op::Dp {
                    op: DpOp::Cmn,
                    s: true,
                    rd: 0,
                    rn: rdn,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
                0b1100 => dp(DpOp::Orr, s),
                0b1101 => Op::Mul {
                    s,
                    rd: rdn,
                    rn: rm,
                    rm: rdn,
                },
                0b1110 => dp(DpOp::Bic, s),
                _ => Op::Dp {
                    op: DpOp::Mvn,
                    s,
                    rd: rdn,
                    rn: 0,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
            }
        }
        // Special data processing, branch and exchange
        0b010001 => {
            let rdn = ((h >> 4) & 8) as u8 | lo3(0);
            let rm = bits(h, 6, 3) as u8;
            match bits(h, 9, 8) {
                // ADD <Rdm>, SP, <Rdm>
                0b00 if rm == 13 => Op::Dp {
                    op: DpOp::Add,
                    s: false,
                    rd: rdn,
                    rn: 13,
                    op2: Operand::reg(rdn),
                    form: DpForm::Three,
                },
                0b00 => Op::Dp {
                    op: DpOp::Add,
                    s: false,
                    rd: rdn,
                    rn: rdn,
                    op2: Operand::reg(rm),
                    form: DpForm::Two,
                },
                0b01 => Op::Dp {
                    op: DpOp::Cmp,
                    s: true,
                    rd: 0,
                    rn: rdn,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
                0b10 => Op::Dp {
                    op: DpOp::Mov,
                    s: false,
                    rd: rdn,
                    rn: 0,
                    op2: Operand::reg(rm),
                    form: DpForm::Three,
                },
                _ => {
                    flag(u, h & 7 != 0); // (0)(0)(0)
                    if bit(h, 7) {
                        Op::Blx { rm }
                    } else {
                        Op::Bx { rm }
                    }
                }
            }
        }
        0b010010 | 0b010011 => Op::Load {
            width: Width::Word,
            signed: false,
            rt: lo3(8),
            addr: Addr::Literal {
                off: ((h & 0xFF) << 2) as i32,
            },
            unpriv: false,
        },
        // Load/store single data item
        0b010100..=0b100111 => decode16_ldst(h),
        0b101000 | 0b101001 => Op::Dp {
            op: DpOp::Add,
            s: false,
            rd: lo3(8),
            rn: 15,
            op2: Operand::imm((h & 0xFF) << 2),
            form: DpForm::Adr,
        },
        0b101010 | 0b101011 => Op::Dp {
            op: DpOp::Add,
            s: false,
            rd: lo3(8),
            rn: 13,
            op2: Operand::imm((h & 0xFF) << 2),
            form: DpForm::Three,
        },
        0b101100..=0b101111 => decode16_misc(h, u),
        0b110000 | 0b110001 => {
            flag(u, h & 0xFF == 0);
            Op::Stm {
                rn: lo3(8),
                regs: (h & 0xFF) as u16,
                wback: true,
                db: false,
            }
        }
        0b110010 | 0b110011 => {
            let rn = lo3(8);
            let regs = (h & 0xFF) as u16;
            flag(u, regs == 0);
            Op::Ldm {
                rn,
                regs,
                wback: regs & (1 << rn) == 0,
                db: false,
            }
        }
        0b110100..=0b110111 => match bits(h, 11, 8) {
            0b1110 => Op::Udf {
                imm: (h & 0xFF) as u16,
            },
            0b1111 => Op::Svc {
                imm: (h & 0xFF) as u8,
            },
            c => Op::B {
                cond: Cond::from_bits(c),
                off: sign_extend((h & 0xFF) << 1, 9),
            },
        },
        0b111000 | 0b111001 => Op::B {
            cond: Cond::Al,
            off: sign_extend((h & 0x7FF) << 1, 12),
        },
        _ => Op::Undefined,
    }
}

fn decode16_ldst(h: u32) -> Op {
    let rt = bits(h, 2, 0) as u8;
    let rn = bits(h, 5, 3) as u8;
    let imm5 = bits(h, 10, 6) as i32;
    let at = |off: i32| Addr::Imm {
        rn,
        off,
        index: true,
        wback: false,
    };
    let load = |width, signed, addr| Op::Load {
        width,
        signed,
        rt,
        addr,
        unpriv: false,
    };
    let store = |width, addr| Op::Store {
        width,
        rt,
        addr,
        unpriv: false,
    };
    match h >> 12 {
        0b0101 => {
            let addr = Addr::Reg {
                rn,
                rm: bits(h, 8, 6) as u8,
                shift: 0,
            };
            match bits(h, 11, 9) {
                0b000 => store(Width::Word, addr),
                0b001 => store(Width::Half, addr),
                0b010 => store(Width::Byte, addr),
                0b011 => load(Width::Byte, true, addr),
                0b100 => load(Width::Word, false, addr),
                0b101 => load(Width::Half, false, addr),
                0b110 => load(Width::Byte, false, addr),
                _ => load(Width::Half, true, addr),
            }
        }
        0b0110 if bit(h, 11) => load(Width::Word, false, at(imm5 << 2)),
        0b0110 => store(Width::Word, at(imm5 << 2)),
        0b0111 if bit(h, 11) => load(Width::Byte, false, at(imm5)),
        0b0111 => store(Width::Byte, at(imm5)),
        0b1000 if bit(h, 11) => load(Width::Half, false, at(imm5 << 1)),
        0b1000 => store(Width::Half, at(imm5 << 1)),
        _ => {
            // SP-relative
            let rt = bits(h, 10, 8) as u8;
            let addr = Addr::Imm {
                rn: 13,
                off: ((h & 0xFF) << 2) as i32,
                index: true,
                wback: false,
            };
            if bit(h, 11) {
                Op::Load {
                    width: Width::Word,
                    signed: false,
                    rt,
                    addr,
                    unpriv: false,
                }
            } else {
                Op::Store {
                    width: Width::Word,
                    rt,
                    addr,
                    unpriv: false,
                }
            }
        }
    }
}

fn decode16_misc(h: u32, u: &mut bool) -> Op {
    let rd = bits(h, 2, 0) as u8;
    let rm = bits(h, 5, 3) as u8;
    match bits(h, 11, 5) {
        0b0000000..=0b0000011 => Op::Dp {
            op: DpOp::Add,
            s: false,
            rd: 13,
            rn: 13,
            op2: Operand::imm((h & 0x7F) << 2),
            form: DpForm::Two,
        },
        0b0000100..=0b0000111 => Op::Dp {
            op: DpOp::Sub,
            s: false,
            rd: 13,
            rn: 13,
            op2: Operand::imm((h & 0x7F) << 2),
            form: DpForm::Two,
        },
        0b0001000..=0b0001111
        | 0b0011000..=0b0011111
        | 0b1001000..=0b1001111
        | 0b1011000..=0b1011111 => Op::Cbz {
            nz: bit(h, 11),
            rn: rd,
            off: ((bits(h, 9, 9) << 6) | (bits(h, 7, 3) << 1)) as u8,
        },
        0b0010000 | 0b0010001 => Op::Extend {
            kind: ExtendKind::Sxth,
            rd,
            rm,
            rot: 0,
        },
        0b0010010 | 0b0010011 => Op::Extend {
            kind: ExtendKind::Sxtb,
            rd,
            rm,
            rot: 0,
        },
        0b0010100 | 0b0010101 => Op::Extend {
            kind: ExtendKind::Uxth,
            rd,
            rm,
            rot: 0,
        },
        0b0010110 | 0b0010111 => Op::Extend {
            kind: ExtendKind::Uxtb,
            rd,
            rm,
            rot: 0,
        },
        0b0100000..=0b0101111 => {
            flag(u, h & 0x1FF == 0);
            let regs = (h & 0xFF) as u16 | if bit(h, 8) { 1 << 14 } else { 0 };
            Op::Stm {
                rn: 13,
                regs,
                wback: true,
                db: true,
            }
        }
        0b0110011 if !bit(h, 3) => Op::Cps {
            disable: bit(h, 4),
            a: bit(h, 2),
            i: bit(h, 1),
            f: bit(h, 0),
        },
        0b1010000 | 0b1010001 => Op::Rev { rd, rm },
        0b1010010 | 0b1010011 => Op::Rev16 { rd, rm },
        0b1010110 | 0b1010111 => Op::Revsh { rd, rm },
        0b1100000..=0b1101111 => {
            flag(u, h & 0x1FF == 0);
            let regs = (h & 0xFF) as u16 | if bit(h, 8) { 1 << 15 } else { 0 };
            Op::Ldm {
                rn: 13,
                regs,
                wback: true,
                db: false,
            }
        }
        0b1110000..=0b1110111 => Op::Bkpt {
            imm: (h & 0xFF) as u8,
        },
        0b1111000..=0b1111111 => {
            let mask = (h & 0xF) as u8;
            if mask != 0 {
                return Op::It {
                    firstcond: bits(h, 7, 4) as u8,
                    mask,
                };
            }
            Op::Hint(match bits(h, 7, 4) {
                0 => Hint::Nop,
                1 => Hint::Yield,
                2 => Hint::Wfe,
                3 => Hint::Wfi,
                4 => Hint::Sev,
                n => Hint::Other(n as u8),
            })
        }
        _ => Op::Undefined,
    }
}

fn decode32(h1: u32, h2: u32, u: &mut bool) -> Op {
    match bits(h1, 12, 11) {
        0b01 => {
            if !bit(h1, 10) {
                if !bit(h1, 9) {
                    if !bit(h1, 6) {
                        decode_ldm_stm(h1, h2, u)
                    } else {
                        decode_dual_excl(h1, h2, u)
                    }
                } else {
                    decode_dp_shifted(h1, h2, u)
                }
            } else {
                Op::Undefined // coprocessor
            }
        }
        0b10 => {
            if bit(h2, 15) {
                decode_branch_misc(h1, h2, u)
            } else if !bit(h1, 9) {
                decode_dp_modified(h1, h2)
            } else {
                decode_dp_plain(h1, h2, u)
            }
        }
        _ => {
            if bit(h1, 10) {
                Op::Undefined // coprocessor
            } else if !bit(h1, 9) {
                decode_ldst32(h1, h2)
            } else if !bit(h1, 8) {
                decode_dp_reg(h1, h2, u)
            } else if !bit(h1, 7) {
                decode_mul(h1, h2)
            } else {
                decode_mul_long(h1, h2)
            }
        }
    }
}

fn decode_ldm_stm(h1: u32, h2: u32, u: &mut bool) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let wback = bit(h1, 5);
    let regs = h2 as u16;
    let load = bit(h1, 4);
    // STM: (0) PC and SP; LDM: (0) SP, and not both PC and LR.
    flag(
        u,
        bit(h2, 13) || (!load && bit(h2, 15)) || (load && bit(h2, 15) && bit(h2, 14)),
    );
    flag(u, regs.count_ones() < 2);
    match bits(h1, 8, 7) {
        0b01 if load => Op::Ldm {
            rn,
            regs,
            wback,
            db: false,
        },
        0b01 => Op::Stm {
            rn,
            regs,
            wback,
            db: false,
        },
        0b10 if load => Op::Ldm {
            rn,
            regs,
            wback,
            db: true,
        },
        0b10 => Op::Stm {
            rn,
            regs,
            wback,
            db: true,
        },
        _ => Op::Undefined, // SRS/RFE: not in ARMv7-M
    }
}

fn decode_dual_excl(h1: u32, h2: u32, u: &mut bool) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let rt = bits(h2, 15, 12) as u8;
    let op1 = bits(h1, 8, 7);
    let op2 = bits(h1, 5, 4);
    match (op1, op2) {
        (0b00, 0b00) => Op::Strex {
            width: Width::Word,
            rd: bits(h2, 11, 8) as u8,
            rt,
            rn,
            off: ((h2 & 0xFF) << 2) as u16,
        },
        (0b00, 0b01) => {
            flag(u, bits(h2, 11, 8) != 0xF);
            Op::Ldrex {
                width: Width::Word,
                rt,
                rn,
                off: ((h2 & 0xFF) << 2) as u16,
            }
        }
        (0b01, 0b00) => {
            flag(u, bits(h2, 11, 8) != 0xF);
            let rd = bits(h2, 3, 0) as u8;
            match bits(h2, 7, 4) {
                0b0100 => Op::Strex {
                    width: Width::Byte,
                    rd,
                    rt,
                    rn,
                    off: 0,
                },
                0b0101 => Op::Strex {
                    width: Width::Half,
                    rd,
                    rt,
                    rn,
                    off: 0,
                },
                _ => Op::Undefined,
            }
        }
        (0b01, 0b01) => {
            let rm = bits(h2, 3, 0) as u8;
            let op3 = bits(h2, 7, 4);
            if op3 <= 1 {
                flag(u, bits(h2, 15, 8) != 0xF0);
            } else {
                flag(u, bits(h2, 11, 8) != 0xF || rm != 0xF);
            }
            match op3 {
                0b0000 => Op::Tb {
                    half: false,
                    rn,
                    rm,
                },
                0b0001 => Op::Tb { half: true, rn, rm },
                0b0100 => Op::Ldrex {
                    width: Width::Byte,
                    rt,
                    rn,
                    off: 0,
                },
                0b0101 => Op::Ldrex {
                    width: Width::Half,
                    rt,
                    rn,
                    off: 0,
                },
                _ => Op::Undefined,
            }
        }
        _ => {
            // LDRD/STRD (immediate), P = op1[1], U = op1[0], W = op2[1]
            let index = bit(h1, 8);
            let up = bit(h1, 7);
            let wback = bit(h1, 5);
            let imm = ((h2 & 0xFF) << 2) as i32;
            let off = if up { imm } else { -imm };
            let rt2 = bits(h2, 11, 8) as u8;
            let addr = if rn == 15 && !wback {
                Addr::Literal { off }
            } else {
                Addr::Imm {
                    rn,
                    off,
                    index,
                    wback,
                }
            };
            if bit(h1, 4) {
                Op::LoadDual { rt, rt2, addr }
            } else {
                Op::StoreDual { rt, rt2, addr }
            }
        }
    }
}

fn dp_op(op: u32, rd: u8, rn: u8, s: bool) -> Option<DpOp> {
    Some(match op {
        0b0000 if rd == 15 && s => DpOp::Tst,
        0b0000 => DpOp::And,
        0b0001 => DpOp::Bic,
        0b0010 if rn == 15 => DpOp::Mov,
        0b0010 => DpOp::Orr,
        0b0011 if rn == 15 => DpOp::Mvn,
        0b0011 => DpOp::Orn,
        0b0100 if rd == 15 && s => DpOp::Teq,
        0b0100 => DpOp::Eor,
        0b1000 if rd == 15 && s => DpOp::Cmn,
        0b1000 => DpOp::Add,
        0b1010 => DpOp::Adc,
        0b1011 => DpOp::Sbc,
        0b1101 if rd == 15 && s => DpOp::Cmp,
        0b1101 => DpOp::Sub,
        0b1110 => DpOp::Rsb,
        _ => return None,
    })
}

fn decode_dp_shifted(h1: u32, h2: u32, u: &mut bool) -> Op {
    flag(u, bit(h2, 15));
    let rn = bits(h1, 3, 0) as u8;
    let rd = bits(h2, 11, 8) as u8;
    let s = bit(h1, 4);
    let shift = Shift::decode(bits(h2, 5, 4), (bits(h2, 14, 12) << 2) | bits(h2, 7, 6));
    let op2 = Operand::Reg {
        rm: bits(h2, 3, 0) as u8,
        shift,
    };
    match dp_op(bits(h1, 8, 5), rd, rn, s) {
        Some(op) => Op::Dp {
            op,
            s,
            rd,
            rn,
            op2,
            form: DpForm::Three,
        },
        None => Op::Undefined,
    }
}

fn decode_dp_modified(h1: u32, h2: u32) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let rd = bits(h2, 11, 8) as u8;
    let s = bit(h1, 4);
    let imm12 = (bits(h1, 10, 10) << 11) | (bits(h2, 14, 12) << 8) | (h2 & 0xFF);
    let op2 = expand_imm(imm12);
    match dp_op(bits(h1, 8, 5), rd, rn, s) {
        Some(op) => Op::Dp {
            op,
            s,
            rd,
            rn,
            op2,
            form: DpForm::Three,
        },
        None => Op::Undefined,
    }
}

fn decode_dp_plain(h1: u32, h2: u32, u: &mut bool) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let rd = bits(h2, 11, 8) as u8;
    let imm12 = (bits(h1, 10, 10) << 11) | (bits(h2, 14, 12) << 8) | (h2 & 0xFF);
    let imm16 = (bits(h1, 3, 0) << 12) | imm12;
    let imm5 = (bits(h2, 14, 12) << 2) | bits(h2, 7, 6);
    let low5 = bits(h2, 4, 0) as u8;
    let op = bits(h1, 8, 4);
    // The bitfield and saturate ops have (0) at i and at hw2 bit 5.
    flag(u, op >= 0b10000 && (bit(h1, 10) || bit(h2, 5)));
    match op {
        0b00000 if rn == 15 => Op::Dp {
            op: DpOp::Add,
            s: false,
            rd,
            rn,
            op2: Operand::imm(imm12),
            form: DpForm::Adr,
        },
        0b00000 => Op::Dp {
            op: DpOp::Add,
            s: false,
            rd,
            rn,
            op2: Operand::imm(imm12),
            form: DpForm::Wide,
        },
        0b00100 => Op::Movw {
            rd,
            imm: imm16 as u16,
        },
        0b01010 if rn == 15 => Op::Dp {
            op: DpOp::Sub,
            s: false,
            rd,
            rn,
            op2: Operand::imm(imm12),
            form: DpForm::Adr,
        },
        0b01010 => Op::Dp {
            op: DpOp::Sub,
            s: false,
            rd,
            rn,
            op2: Operand::imm(imm12),
            form: DpForm::Wide,
        },
        0b01100 => Op::Movt {
            rd,
            imm: imm16 as u16,
        },
        0b10000 | 0b10010 => {
            if bit(h1, 5) && imm5 == 0 {
                return Op::Undefined; // SSAT16: not in ARMv7-M
            }
            Op::Sat {
                signed: true,
                rd,
                sat: low5 + 1,
                rn,
                shift: Shift::decode(bits(h1, 5, 5) << 1, imm5),
            }
        }
        0b11000 | 0b11010 => {
            if bit(h1, 5) && imm5 == 0 {
                return Op::Undefined; // USAT16
            }
            Op::Sat {
                signed: false,
                rd,
                sat: low5,
                rn,
                shift: Shift::decode(bits(h1, 5, 5) << 1, imm5),
            }
        }
        0b10100 => Op::Bfx {
            signed: true,
            rd,
            rn,
            lsb: imm5 as u8,
            width: low5 + 1,
        },
        0b11100 => Op::Bfx {
            signed: false,
            rd,
            rn,
            lsb: imm5 as u8,
            width: low5 + 1,
        },
        0b10110 => {
            // msb < lsb is UNPREDICTABLE; LLVM shows it as lsb = msb, width 1.
            let (lsb, width) = if low5 < imm5 as u8 {
                (low5, 1)
            } else {
                (imm5 as u8, low5 + 1 - imm5 as u8)
            };
            if rn == 15 {
                Op::Bfc { rd, lsb, width }
            } else {
                Op::Bfi { rd, rn, lsb, width }
            }
        }
        _ => Op::Undefined,
    }
}

fn decode_branch_misc(h1: u32, h2: u32, u: &mut bool) -> Op {
    let op = bits(h1, 10, 4);
    let s = bit(h1, 10) as u32;
    let j1 = bits(h2, 13, 13);
    let j2 = bits(h2, 11, 11);
    match (bit(h2, 14), bit(h2, 12)) {
        (_, true) => {
            // B T4 / BL
            let i1 = !(j1 ^ s) & 1;
            let i2 = !(j2 ^ s) & 1;
            let imm = (s << 24)
                | (i1 << 23)
                | (i2 << 22)
                | (bits(h1, 9, 0) << 12)
                | (bits(h2, 10, 0) << 1);
            let off = sign_extend(imm, 25);
            if bit(h2, 14) {
                Op::Bl { off }
            } else {
                Op::B {
                    cond: Cond::Al,
                    off,
                }
            }
        }
        (false, false) if bits(op, 5, 3) != 0b111 => {
            let imm = (s << 20)
                | (j2 << 19)
                | (j1 << 18)
                | (bits(h1, 5, 0) << 12)
                | (bits(h2, 10, 0) << 1);
            Op::B {
                cond: Cond::from_bits(bits(h1, 9, 6)),
                off: sign_extend(imm, 21),
            }
        }
        (false, false) => match op {
            0b0111000 | 0b0111001 => {
                flag(
                    u,
                    bit(h2, 13) || bits(h2, 9, 8) != 0 || !valid_sysm(h2 & 0xFF),
                );
                msr(h1, h2)
            }
            0b0111010 => {
                flag(
                    u,
                    bits(h1, 3, 0) != 0xF || bit(h2, 13) || bits(h2, 10, 8) != 0,
                );
                Op::Hint(match h2 & 0xFF {
                    0 => Hint::Nop,
                    1 => Hint::Yield,
                    2 => Hint::Wfe,
                    3 => Hint::Wfi,
                    4 => Hint::Sev,
                    n => Hint::Other(n as u8),
                })
            }
            0b0111011 => {
                flag(
                    u,
                    bits(h1, 3, 0) != 0xF || bit(h2, 13) || bits(h2, 11, 8) != 0xF,
                );
                let opt = (h2 & 0xF) as u8;
                match bits(h2, 7, 4) {
                    0b0010 => Op::Clrex,
                    0b0100 => Op::Barrier {
                        kind: Barrier::Dsb,
                        opt,
                    },
                    0b0101 => Op::Barrier {
                        kind: Barrier::Dmb,
                        opt,
                    },
                    0b0110 => Op::Barrier {
                        kind: Barrier::Isb,
                        opt,
                    },
                    _ => Op::Undefined,
                }
            }
            0b0111110 | 0b0111111 => {
                flag(
                    u,
                    bits(h1, 3, 0) != 0xF || bit(h2, 13) || !valid_sysm(h2 & 0xFF),
                );
                Op::Mrs {
                    rd: bits(h2, 11, 8) as u8,
                    sysm: (h2 & 0xFF) as u8,
                }
            }
            // op1 = 010: permanently UNDEFINED
            0b1111111 if bit(h2, 13) => Op::Udf {
                imm: ((bits(h1, 3, 0) << 12) | bits(h2, 11, 0)) as u16,
            },
            _ => Op::Undefined,
        },
        // op1 = 1x0: BLX (immediate), which ARMv7-M doesn't have
        _ => Op::Undefined,
    }
}

fn msr(h1: u32, h2: u32) -> Op {
    Op::Msr {
        rn: bits(h1, 3, 0) as u8,
        sysm: (h2 & 0xFF) as u8,
        mask: bits(h2, 11, 10) as u8,
    }
}

/// The special registers ARMv7-M has (MRS/MSR SYSm).
fn valid_sysm(sysm: u32) -> bool {
    matches!(sysm, 0..=3 | 5..=9 | 16..=20)
}

fn decode_ldst32(h1: u32, h2: u32) -> Op {
    let signed = bit(h1, 8);
    let up12 = bit(h1, 7);
    let load = bit(h1, 4);
    let rn = bits(h1, 3, 0) as u8;
    let rt = bits(h2, 15, 12) as u8;
    let width = match bits(h1, 6, 5) {
        0 => Width::Byte,
        1 => Width::Half,
        2 => Width::Word,
        _ => return Op::Undefined,
    };
    if !load && signed {
        return Op::Undefined;
    }
    if signed && width == Width::Word {
        return Op::Undefined;
    }
    let mut unpriv = false;
    let addr = if rn == 15 {
        if !load {
            return Op::Undefined;
        }
        let imm = (h2 & 0xFFF) as i32;
        Addr::Literal {
            off: if up12 { imm } else { -imm },
        }
    } else if up12 {
        Addr::Imm {
            rn,
            off: (h2 & 0xFFF) as i32,
            index: true,
            wback: false,
        }
    } else if bit(h2, 11) {
        let index = bit(h2, 10);
        let up = bit(h2, 9);
        let wback = bit(h2, 8);
        let imm = (h2 & 0xFF) as i32;
        if index && up && !wback {
            unpriv = true;
        } else if !index && !wback {
            return Op::Undefined;
        }
        Addr::Imm {
            rn,
            off: if up { imm } else { -imm },
            index,
            wback,
        }
    } else if bits(h2, 11, 6) == 0 {
        Addr::Reg {
            rn,
            rm: bits(h2, 3, 0) as u8,
            shift: bits(h2, 5, 4) as u8,
        }
    } else {
        return Op::Undefined;
    };
    if load && rt == 15 && width != Width::Word {
        // PLD/PLI take [rn, #imm12], [rn, #-imm8], [rn, rm, lsl #n] and the
        // literal; the halfword forms are unallocated hints. Other forms
        // (writeback, unprivileged) stay loads to PC, as LLVM shows them.
        let hint_form = match addr {
            Addr::Imm {
                off,
                index: true,
                wback: false,
                ..
            } => !unpriv && (up12 || off < 0 || bit(h2, 11) && !bit(h2, 9)),
            Addr::Reg { .. } | Addr::Literal { .. } => true,
            _ => false,
        };
        if hint_form {
            return if width == Width::Byte {
                Op::Preload {
                    instr: signed,
                    addr,
                }
            } else {
                Op::Undefined
            };
        }
    }
    if load {
        Op::Load {
            width,
            signed,
            rt,
            addr,
            unpriv,
        }
    } else {
        Op::Store {
            width,
            rt,
            addr,
            unpriv,
        }
    }
}

fn decode_dp_reg(h1: u32, h2: u32, u: &mut bool) -> Op {
    if bits(h2, 15, 12) != 0b1111 {
        return Op::Undefined;
    }
    let rn = bits(h1, 3, 0) as u8;
    let rd = bits(h2, 11, 8) as u8;
    let rm = bits(h2, 3, 0) as u8;
    let op1 = bits(h1, 7, 4);
    let op2 = bits(h2, 7, 4);
    // REV/REV16/RBIT/REVSH/CLZ give Rm twice, and the two must agree.
    flag(u, op1 >> 2 == 0b10 && rn != rm);
    match (op1, op2) {
        (0b0000..=0b0111, 0b0000) => {
            let kind = [
                ShiftKind::Lsl,
                ShiftKind::Lsr,
                ShiftKind::Asr,
                ShiftKind::Ror,
            ][(op1 >> 1) as usize];
            Op::ShiftReg {
                kind,
                s: bit(h1, 4),
                rd,
                rn,
                rm,
                two: false,
            }
        }
        (0b0000 | 0b0001 | 0b0100 | 0b0101, 0b1000..=0b1111) if rn == 15 => {
            let kind = match op1 {
                0b0000 => ExtendKind::Sxth,
                0b0001 => ExtendKind::Uxth,
                0b0100 => ExtendKind::Sxtb,
                _ => ExtendKind::Uxtb,
            };
            Op::Extend {
                kind,
                rd,
                rm,
                rot: (bits(h2, 5, 4) * 8) as u8,
            }
        }
        (0b1001, 0b1000) => Op::Rev { rd, rm },
        (0b1001, 0b1001) => Op::Rev16 { rd, rm },
        (0b1001, 0b1010) => Op::Rbit { rd, rm },
        (0b1001, 0b1011) => Op::Revsh { rd, rm },
        (0b1011, 0b1000) => Op::Clz { rd, rm },
        _ => Op::Undefined,
    }
}

fn decode_mul(h1: u32, h2: u32) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let ra = bits(h2, 15, 12) as u8;
    let rd = bits(h2, 11, 8) as u8;
    let rm = bits(h2, 3, 0) as u8;
    match (bits(h1, 6, 4), bits(h2, 7, 4)) {
        (0, 0) if ra == 15 => Op::Mul {
            s: false,
            rd,
            rn,
            rm,
        },
        (0, 0) => Op::Mla { rd, rn, rm, ra },
        (0, 1) => Op::Mls { rd, rn, rm, ra },
        _ => Op::Undefined,
    }
}

fn decode_mul_long(h1: u32, h2: u32) -> Op {
    let rn = bits(h1, 3, 0) as u8;
    let rdlo = bits(h2, 15, 12) as u8;
    let rdhi = bits(h2, 11, 8) as u8;
    let rm = bits(h2, 3, 0) as u8;
    let op2 = bits(h2, 7, 4);
    let long = |signed, acc| Op::MulLong {
        signed,
        acc,
        rdlo,
        rdhi,
        rn,
        rm,
    };
    match (bits(h1, 6, 4), op2) {
        (0b000, 0) => long(true, false),
        (0b001, 0b1111) if rdlo == 15 => Op::Div {
            signed: true,
            rd: rdhi,
            rn,
            rm,
        },
        (0b010, 0) => long(false, false),
        (0b011, 0b1111) if rdlo == 15 => Op::Div {
            signed: false,
            rd: rdhi,
            rn,
            rm,
        },
        (0b100, 0) => long(true, true),
        (0b110, 0) => long(false, true),
        _ => Op::Undefined,
    }
}

/// ITSTATE (EPSR bits [15:10] and [26:25] together): the condition in the
/// top four bits, the remaining-instructions mask below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItState(pub u8);

impl ItState {
    pub fn start(firstcond: u8, mask: u8) -> ItState {
        ItState((firstcond << 4) | mask)
    }

    pub fn in_block(self) -> bool {
        self.0 & 0xF != 0
    }

    /// The condition on the current instruction, if it's in a block.
    pub fn cond(self) -> Option<Cond> {
        self.in_block()
            .then(|| Cond::from_bits((self.0 >> 4) as u32))
    }

    /// Whether the current instruction is the block's last.
    pub fn last(self) -> bool {
        self.0 & 0xF == 0b1000
    }

    /// ITAdvance, after each instruction of the block.
    pub fn advance(&mut self) {
        if self.0 & 0x7 == 0 {
            self.0 = 0;
        } else {
            self.0 = (self.0 & 0xE0) | ((self.0 << 1) & 0x1F);
        }
    }
}
