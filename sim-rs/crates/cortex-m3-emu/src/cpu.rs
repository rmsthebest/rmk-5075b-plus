//! The processor: registers, the step loop and instruction semantics (ARM
//! DDI 0403E chapter A7's pseudocode). Exceptions are in `exception.rs`, the
//! system control space in `ppb.rs`.

use crate::bus::{Bus, Size};
use crate::decode::*;
use crate::exception::{Exc, Fault};
use crate::ppb::Ppb;
use crate::{Config, CoreDiag};

/// Flags in APSR.
pub const N: u32 = 1 << 31;
pub const Z: u32 = 1 << 30;
pub const C: u32 = 1 << 29;
pub const V: u32 = 1 << 28;
pub const Q: u32 = 1 << 27;

pub const SP: usize = 13;
pub const LR: usize = 14;
pub const PC: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sleep {
    Awake,
    Wfi,
    Wfe,
}

pub struct Cpu {
    pub config: Config,
    /// R0-R15. R13 is the SP in use; the other one is in `other_sp`.
    pub r: [u32; 16],
    pub other_sp: u32,
    /// N, Z, C, V, Q in bits 31..27.
    pub apsr: u32,
    /// The exception being handled; 0 in Thread mode.
    pub ipsr: u16,
    /// EPSR.T: clear means the next instruction faults (INVSTATE).
    pub thumb: bool,
    pub it: ItState,
    pub primask: bool,
    pub faultmask: bool,
    pub basepri: u8,
    /// bit 0 nPRIV, bit 1 SPSEL
    pub control: u8,
    pub sleep: Sleep,
    pub event: bool,
    /// LDREX's address, until STREX, CLREX or an exception.
    pub monitor: Option<u32>,
    /// Stopped: a lockup, or a fault the firmware can't take.
    pub halted: bool,
    /// AIRCR.SYSRESETREQ was written; whoever runs the core resets the
    /// system (the core alone with `reset`).
    pub reset_requested: bool,
    pub ppb: Ppb,
    /// Core ticks since power-on (TICK_HZ).
    pub now: u64,
    /// Instructions executed (each counts once, taken or skipped by IT).
    pub instructions: u64,
    /// Times the core went to sleep.
    pub sleeps: u64,
    pub diags: Vec<CoreDiag>,
    /// Lockstep with another simulator's trace: asynchronous exceptions
    /// are taken only through `take_exception`, where the other one took
    /// them, and a sleeping core waits to be woken the same way.
    pub lockstep: bool,
    /// In lockstep, the PPB addresses read (the caller empties it).
    pub ppb_reads: Vec<u32>,
    /// The address of the instruction executing (for faults and diags).
    pub(crate) cur_pc: u32,
    /// The IT state it executes in.
    pub(crate) cur_it: ItState,
    /// A synchronous exception the instruction raised: (exception, return
    /// address), entered once the instruction is done.
    pub(crate) pending_sync: Option<(u16, u32)>,
    /// Set when an instruction wrote the PC.
    branched: bool,
}

impl Cpu {
    pub fn new(config: Config) -> Self {
        Cpu {
            config,
            r: [0; 16],
            other_sp: 0,
            apsr: 0,
            ipsr: 0,
            thumb: true,
            it: ItState::default(),
            primask: false,
            faultmask: false,
            basepri: 0,
            control: 0,
            sleep: Sleep::Awake,
            event: false,
            monitor: None,
            halted: false,
            reset_requested: false,
            ppb: Ppb::new(config),
            now: 0,
            instructions: 0,
            sleeps: 0,
            diags: Vec::new(),
            lockstep: false,
            ppb_reads: Vec::new(),
            cur_pc: 0,
            cur_it: ItState::default(),
            pending_sync: None,
            branched: false,
        }
    }

    /// A system reset: everything in the core back to its reset value, then
    /// SP and PC from the vector table at 0. Time, counters and diags carry on.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        let keep = (
            self.now,
            self.instructions,
            self.sleeps,
            std::mem::take(&mut self.diags),
            self.lockstep,
        );
        *self = Cpu::new(self.config);
        (
            self.now,
            self.instructions,
            self.sleeps,
            self.diags,
            self.lockstep,
        ) = keep;
        self.ppb.reset(self.now);
        self.r[LR] = 0xFFFF_FFFF;
        self.r[SP] = bus.read(0, Size::Word) & !3;
        let pc = bus.read(4, Size::Word);
        self.thumb = pc & 1 != 0;
        self.r[PC] = pc & !1;
    }

    pub fn pc(&self) -> u32 {
        self.r[PC]
    }

    /// The address of the last instruction executed.
    pub fn last_pc(&self) -> u32 {
        self.cur_pc
    }

    /// Enters exception `exc` now, returning to the current PC (lockstep).
    pub fn take_exception(&mut self, bus: &mut impl Bus, exc: u16) {
        let ret = self.r[PC];
        self.enter(bus, exc, ret);
    }

    pub fn handler_mode(&self) -> bool {
        self.ipsr != 0
    }

    pub fn privileged(&self) -> bool {
        self.handler_mode() || self.control & 1 == 0
    }

    /// The main stack pointer, wherever it is banked.
    pub fn msp(&self) -> u32 {
        if self.psp_active() {
            self.other_sp
        } else {
            self.r[SP]
        }
    }

    pub fn psp(&self) -> u32 {
        if self.psp_active() {
            self.r[SP]
        } else {
            self.other_sp
        }
    }

    pub fn set_msp(&mut self, v: u32) {
        if self.psp_active() {
            self.other_sp = v & !3
        } else {
            self.r[SP] = v & !3
        }
    }

    pub fn set_psp(&mut self, v: u32) {
        if self.psp_active() {
            self.r[SP] = v & !3
        } else {
            self.other_sp = v & !3
        }
    }

    pub(crate) fn psp_active(&self) -> bool {
        !self.handler_mode() && self.control & 2 != 0
    }

    /// Changes mode or SPSEL, swapping the banked SP when the selection changes.
    pub(crate) fn set_stack_selection(&mut self, ipsr: u16, control: u8) {
        let before = self.psp_active();
        self.ipsr = ipsr;
        self.control = control;
        if before != self.psp_active() {
            std::mem::swap(&mut self.r[SP], &mut self.other_sp);
        }
    }

    pub fn xpsr(&self) -> u32 {
        let it = self.it.0 as u32;
        self.apsr
            | self.ipsr as u32
            | (self.thumb as u32) << 24
            | (it & 0x3) << 25
            | (it >> 2) << 10
    }

    fn flag(&self, f: u32) -> bool {
        self.apsr & f != 0
    }

    fn set_nz(&mut self, v: u32) {
        self.apsr = (self.apsr & !(N | Z)) | (v & N) | if v == 0 { Z } else { 0 };
    }

    fn set_c(&mut self, c: bool) {
        self.apsr = (self.apsr & !C) | if c { C } else { 0 };
    }

    fn set_v(&mut self, v: bool) {
        self.apsr = (self.apsr & !V) | if v { V } else { 0 };
    }

    pub fn cond_passes(&self, cond: Cond) -> bool {
        let (n, z, c, v) = (self.flag(N), self.flag(Z), self.flag(C), self.flag(V));
        match cond {
            Cond::Eq => z,
            Cond::Ne => !z,
            Cond::Hs => c,
            Cond::Lo => !c,
            Cond::Mi => n,
            Cond::Pl => !n,
            Cond::Vs => v,
            Cond::Vc => !v,
            Cond::Hi => c && !z,
            Cond::Ls => !c || z,
            Cond::Ge => n == v,
            Cond::Lt => n != v,
            Cond::Gt => !z && n == v,
            Cond::Le => z || n != v,
            Cond::Al => true,
        }
    }

    /// A register as an operand: PC reads as the instruction's address + 4.
    fn reg(&self, r: u8) -> u32 {
        if r == 15 {
            self.cur_pc.wrapping_add(4)
        } else {
            self.r[r as usize]
        }
    }

    fn set_reg(&mut self, r: u8, v: u32) {
        if r == 15 {
            self.branch(v);
        } else if r == 13 {
            self.r[SP] = v & !3;
        } else {
            self.r[r as usize] = v;
        }
    }

    /// BranchWritePC.
    fn branch(&mut self, target: u32) {
        self.r[PC] = target & !1;
        self.branched = true;
    }

    /// BXWritePC / LoadWritePC: interworking, and exception return from
    /// Handler mode.
    fn bx_write_pc(&mut self, bus: &mut impl Bus, target: u32) {
        if self.handler_mode() && target >> 28 == 0xF {
            self.exception_return(bus, target);
            self.branched = true;
            return;
        }
        self.thumb = target & 1 != 0;
        self.branch(target);
    }

    pub(crate) fn diag(&mut self, d: CoreDiag) {
        self.diags.push(d);
    }

    // ---- memory ----

    pub(crate) fn read(&mut self, bus: &mut impl Bus, addr: u32, size: Size) -> u32 {
        if (0xE000_0000..0xE010_0000).contains(&addr) {
            let now = self.now;
            self.ppb.ipsr = self.ipsr;
            if self.lockstep {
                self.ppb_reads.push(addr);
            }
            let v = self.ppb.read(addr, size, now, &mut self.diags);
            return self.ppb_side_effects(v);
        }
        bus.read(addr, size)
    }

    pub(crate) fn write(&mut self, bus: &mut impl Bus, addr: u32, size: Size, value: u32) {
        if (0xE000_0000..0xE010_0000).contains(&addr) {
            let now = self.now;
            bus.ppb_write(addr, size, value);
            self.ppb.write(addr, size, value, now, &mut self.diags);
            self.ppb_side_effects(0);
            return;
        }
        bus.write(addr, size, value)
    }

    /// Applies what a PPB access asked of the core itself.
    fn ppb_side_effects(&mut self, v: u32) -> u32 {
        if std::mem::take(&mut self.ppb.reset_request) {
            self.reset_requested = true;
        }
        v
    }

    /// Unaligned word/halfword accesses are allowed for LDR/STR(H), unless
    /// CCR.UNALIGN_TRP; LDM/STM/LDRD/STRD/LDREX always need alignment.
    fn aligned(&mut self, addr: u32, size: Size, strict: bool) -> bool {
        let ok = addr % size.bytes() == 0 || (!strict && !self.ppb.unalign_trp());
        if !ok {
            self.fault(Fault::Unaligned);
        }
        ok
    }

    // ---- running ----

    /// Runs until `deadline` (core ticks) or a halt or reset request.
    pub fn run_until(&mut self, bus: &mut impl Bus, deadline: u64) {
        while self.now < deadline && !self.halted && !self.reset_requested {
            if self.sleep != Sleep::Awake {
                self.sleep_until(bus, deadline);
            } else {
                self.step(bus);
            }
        }
    }

    /// One instruction, or one exception entry.
    pub fn step(&mut self, bus: &mut impl Bus) {
        self.step_inner(bus);
        self.take_sync(bus);
    }

    fn step_inner(&mut self, bus: &mut impl Bus) {
        bus.advance(self.now);
        self.ppb.tick(self.now);
        self.update_irqs(bus);
        if !self.lockstep && self.take_pending(bus) {
            return;
        }
        self.cur_pc = self.r[PC];
        self.cur_it = self.it;
        bus.executing(self.cur_pc);
        if !self.thumb {
            self.fault(Fault::InvState);
            return;
        }
        let Some(hw1) = bus.fetch(self.cur_pc) else {
            self.fault(Fault::IBusErr);
            return;
        };
        let wide = is_wide(hw1);
        let hw2 = if wide {
            match bus.fetch(self.cur_pc.wrapping_add(2)) {
                Some(h) => h,
                None => {
                    self.fault(Fault::IBusErr);
                    return;
                }
            }
        } else {
            0
        };
        let insn = decode(hw1, hw2, self.it.in_block());
        self.instructions += 1;
        self.now += self.config.insn_ticks;
        self.r[PC] = self.cur_pc.wrapping_add(insn.len as u32);
        self.branched = false;
        if insn.unpredictable {
            self.diag(CoreDiag::Unpredictable {
                pc: self.cur_pc,
                insn,
            });
        }
        // In an IT block the block's condition decides; outside, Al. The
        // state advances first, so an SVC stacks the next instruction's;
        // a fault puts `cur_it` back, as the instruction runs again.
        let run = match self.it.cond() {
            Some(c) => self.cond_passes(c),
            None => true,
        };
        self.cur_it = self.it;
        self.it.advance();
        if run {
            self.execute(bus, insn);
        }
    }

    /// Asleep: jump to the next thing that can wake the core, or the deadline.
    fn sleep_until(&mut self, bus: &mut impl Bus, deadline: u64) {
        bus.advance(self.now);
        self.ppb.tick(self.now);
        self.update_irqs(bus);
        if self.should_wake() {
            self.sleep = Sleep::Awake;
            return;
        }
        let next = bus.next_event().min(self.ppb.next_event()).min(deadline);
        self.now = next.max(self.now + 1);
    }

    fn execute(&mut self, bus: &mut impl Bus, insn: Insn) {
        match insn.op {
            Op::Dp {
                op,
                s,
                rd,
                rn,
                op2,
                form,
            } => self.exec_dp(op, s, rd, rn, op2, form),
            Op::ShiftReg {
                kind,
                s,
                rd,
                rn,
                rm,
                ..
            } => {
                let amount = self.reg(rm) & 0xFF;
                let (v, c) = shift_c(self.reg(rn), kind, amount, self.flag(C));
                self.set_reg(rd, v);
                if s {
                    self.set_nz(v);
                    self.set_c(c);
                }
            }
            Op::Movw { rd, imm } => self.set_reg(rd, imm as u32),
            Op::Movt { rd, imm } => {
                let v = (self.reg(rd) & 0xFFFF) | (imm as u32) << 16;
                self.set_reg(rd, v)
            }
            Op::Mul { s, rd, rn, rm } => {
                let v = self.reg(rn).wrapping_mul(self.reg(rm));
                self.set_reg(rd, v);
                if s {
                    self.set_nz(v);
                }
            }
            Op::Mla { rd, rn, rm, ra } => {
                let v = self
                    .reg(rn)
                    .wrapping_mul(self.reg(rm))
                    .wrapping_add(self.reg(ra));
                self.set_reg(rd, v)
            }
            Op::Mls { rd, rn, rm, ra } => {
                let v = self
                    .reg(ra)
                    .wrapping_sub(self.reg(rn).wrapping_mul(self.reg(rm)));
                self.set_reg(rd, v)
            }
            Op::MulLong {
                signed,
                acc,
                rdlo,
                rdhi,
                rn,
                rm,
            } => {
                let (a, b) = (self.reg(rn), self.reg(rm));
                let mut v = if signed {
                    (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64
                } else {
                    (a as u64) * (b as u64)
                };
                if acc {
                    v = v.wrapping_add((self.reg(rdhi) as u64) << 32 | self.reg(rdlo) as u64);
                }
                self.set_reg(rdlo, v as u32);
                self.set_reg(rdhi, (v >> 32) as u32);
            }
            Op::Div { signed, rd, rn, rm } => {
                let (n, m) = (self.reg(rn), self.reg(rm));
                if m == 0 {
                    if self.ppb.div_0_trp() {
                        self.fault(Fault::DivByZero);
                        return;
                    }
                    self.set_reg(rd, 0);
                } else if signed {
                    self.set_reg(rd, (n as i32).wrapping_div(m as i32) as u32);
                } else {
                    self.set_reg(rd, n / m);
                }
            }
            Op::Extend { kind, rd, rm, rot } => {
                let v = self.reg(rm).rotate_right(rot as u32);
                let v = match kind {
                    ExtendKind::Sxtb => v as u8 as i8 as i32 as u32,
                    ExtendKind::Sxth => v as u16 as i16 as i32 as u32,
                    ExtendKind::Uxtb => v & 0xFF,
                    ExtendKind::Uxth => v & 0xFFFF,
                };
                self.set_reg(rd, v)
            }
            Op::Clz { rd, rm } => self.set_reg(rd, self.reg(rm).leading_zeros()),
            Op::Rev { rd, rm } => self.set_reg(rd, self.reg(rm).swap_bytes()),
            Op::Rev16 { rd, rm } => {
                let v = self.reg(rm);
                self.set_reg(rd, ((v & 0x00FF_00FF) << 8) | ((v >> 8) & 0x00FF_00FF))
            }
            Op::Revsh { rd, rm } => {
                let v = (self.reg(rm) as u16).swap_bytes();
                self.set_reg(rd, v as i16 as i32 as u32)
            }
            Op::Rbit { rd, rm } => self.set_reg(rd, self.reg(rm).reverse_bits()),
            Op::Bfi { rd, rn, lsb, width } => {
                let mask = (((1u64 << width) - 1) << lsb) as u32;
                let v = (self.reg(rd) & !mask) | ((self.reg(rn) << lsb) & mask);
                self.set_reg(rd, v)
            }
            Op::Bfc { rd, lsb, width } => {
                let mask = (((1u64 << width) - 1) << lsb) as u32;
                self.set_reg(rd, self.reg(rd) & !mask)
            }
            Op::Bfx {
                signed,
                rd,
                rn,
                lsb,
                width,
            } => {
                let v = self.reg(rn) >> lsb;
                let v = if width >= 32 {
                    v
                } else if signed {
                    (((v << (32 - width)) as i32) >> (32 - width)) as u32
                } else {
                    v & ((1 << width) - 1)
                };
                self.set_reg(rd, v)
            }
            Op::Sat {
                signed,
                rd,
                sat,
                rn,
                shift,
            } => {
                let (v, _) = shift_c(self.reg(rn), shift.kind, shift.amount as u32, self.flag(C));
                let v = v as i32 as i64;
                let (lo, hi) = if signed {
                    (-(1i64 << (sat - 1)), (1i64 << (sat - 1)) - 1)
                } else {
                    (0, (1i64 << sat) - 1)
                };
                let r = v.clamp(lo, hi);
                if r != v {
                    self.apsr |= Q;
                }
                self.set_reg(rd, r as u32)
            }

            Op::Load {
                width,
                signed,
                rt,
                addr,
                ..
            } => {
                let (a, wb) = self.address(addr);
                let size = size_of(width);
                if !self.aligned(a, size, false) {
                    return;
                }
                let raw = self.read(bus, a, size);
                let v = match (width, signed) {
                    (Width::Byte, true) => raw as u8 as i8 as i32 as u32,
                    (Width::Half, true) => raw as u16 as i16 as i32 as u32,
                    _ => raw,
                };
                if let Some((rn, v)) = wb {
                    self.set_reg(rn, v);
                }
                if rt == 15 {
                    if a & 3 != 0 {
                        self.diag(CoreDiag::Unpredictable {
                            pc: self.cur_pc,
                            insn,
                        });
                    }
                    self.bx_write_pc(bus, v);
                } else {
                    self.set_reg(rt, v);
                }
            }
            Op::Store {
                width, rt, addr, ..
            } => {
                let (a, wb) = self.address(addr);
                let size = size_of(width);
                if !self.aligned(a, size, false) {
                    return;
                }
                let v = self.reg(rt);
                self.write(bus, a, size, v);
                if let Some((rn, v)) = wb {
                    self.set_reg(rn, v);
                }
            }
            Op::LoadDual { rt, rt2, addr } => {
                let (a, wb) = self.address(addr);
                if !self.aligned(a, Size::Word, true) {
                    return;
                }
                let lo = self.read(bus, a, Size::Word);
                let hi = self.read(bus, a.wrapping_add(4), Size::Word);
                self.set_reg(rt, lo);
                self.set_reg(rt2, hi);
                if let Some((rn, v)) = wb {
                    self.set_reg(rn, v);
                }
            }
            Op::StoreDual { rt, rt2, addr } => {
                let (a, wb) = self.address(addr);
                if !self.aligned(a, Size::Word, true) {
                    return;
                }
                let (lo, hi) = (self.reg(rt), self.reg(rt2));
                self.write(bus, a, Size::Word, lo);
                self.write(bus, a.wrapping_add(4), Size::Word, hi);
                if let Some((rn, v)) = wb {
                    self.set_reg(rn, v);
                }
            }
            Op::Ldm {
                rn,
                regs,
                wback,
                db,
            } => {
                let n = regs.count_ones();
                let base = self.reg(rn);
                let start = if db { base.wrapping_sub(4 * n) } else { base };
                if !self.aligned(start, Size::Word, true) {
                    return;
                }
                let mut a = start;
                let mut pc = None;
                for r in 0..16u8 {
                    if regs & (1 << r) != 0 {
                        let v = self.read(bus, a, Size::Word);
                        if r == 15 {
                            pc = Some(v)
                        } else {
                            self.set_reg(r, v)
                        }
                        a = a.wrapping_add(4);
                    }
                }
                if wback && regs & (1 << rn) == 0 {
                    let end = if db { start } else { base.wrapping_add(4 * n) };
                    self.set_reg(rn, end);
                }
                if let Some(v) = pc {
                    self.bx_write_pc(bus, v);
                }
            }
            Op::Stm {
                rn,
                regs,
                wback,
                db,
            } => {
                let n = regs.count_ones();
                let base = self.reg(rn);
                let start = if db { base.wrapping_sub(4 * n) } else { base };
                if !self.aligned(start, Size::Word, true) {
                    return;
                }
                let mut a = start;
                for r in 0..16u8 {
                    if regs & (1 << r) != 0 {
                        let v = self.reg(r);
                        self.write(bus, a, Size::Word, v);
                        a = a.wrapping_add(4);
                    }
                }
                if wback {
                    let end = if db { start } else { base.wrapping_add(4 * n) };
                    self.set_reg(rn, end);
                }
            }
            Op::Ldrex { width, rt, rn, off } => {
                let a = self.reg(rn).wrapping_add(off as u32);
                let size = size_of(width);
                if !self.aligned(a, size, true) {
                    return;
                }
                let v = self.read(bus, a, size);
                self.monitor = Some(a);
                self.set_reg(rt, v);
            }
            Op::Strex {
                width,
                rd,
                rt,
                rn,
                off,
            } => {
                let a = self.reg(rn).wrapping_add(off as u32);
                let size = size_of(width);
                if !self.aligned(a, size, true) {
                    return;
                }
                if self.monitor.take() == Some(a) {
                    let v = self.reg(rt);
                    self.write(bus, a, size, v);
                    self.set_reg(rd, 0);
                } else {
                    self.set_reg(rd, 1);
                }
            }
            Op::Clrex => self.monitor = None,

            Op::B { cond, off } => {
                if self.cond_passes(cond) {
                    self.branch(self.reg(15).wrapping_add(off as u32));
                }
            }
            Op::Bl { off } => {
                self.r[LR] = self.r[PC] | 1;
                self.branch(self.reg(15).wrapping_add(off as u32));
            }
            Op::Bx { rm } => {
                let t = self.reg(rm);
                self.bx_write_pc(bus, t);
            }
            Op::Blx { rm } => {
                let t = self.reg(rm);
                self.r[LR] = self.r[PC] | 1;
                self.bx_write_pc(bus, t);
            }
            Op::Cbz { nz, rn, off } => {
                if (self.reg(rn) != 0) == nz {
                    self.branch(self.reg(15).wrapping_add(off as u32));
                }
            }
            Op::Tb { half, rn, rm } => {
                let base = self.reg(rn);
                let idx = self.reg(rm);
                let units = if half {
                    self.read(bus, base.wrapping_add(idx << 1), Size::Half)
                } else {
                    self.read(bus, base.wrapping_add(idx), Size::Byte)
                };
                self.branch(self.reg(15).wrapping_add(units * 2));
            }

            Op::It { firstcond, mask } => {
                if self.it.in_block() {
                    self.diag(CoreDiag::Unpredictable {
                        pc: self.cur_pc,
                        insn,
                    });
                }
                self.it = ItState::start(firstcond, mask);
            }
            Op::Hint(h) => match h {
                Hint::Wfi => self.enter_sleep(bus, Sleep::Wfi),
                Hint::Wfe => {
                    if self.event {
                        self.event = false;
                    } else {
                        self.enter_sleep(bus, Sleep::Wfe);
                    }
                }
                Hint::Sev => self.event = true,
                Hint::Nop | Hint::Yield | Hint::Other(_) => {}
            },
            Op::Preload { .. } | Op::Barrier { .. } => {}
            Op::Mrs { rd, sysm } => {
                let v = self.read_special(sysm);
                self.set_reg(rd, v);
            }
            Op::Msr { rn, sysm, mask } => {
                let v = self.reg(rn);
                self.write_special(sysm, mask, v);
            }
            Op::Cps { disable, i, f, .. } => {
                if self.privileged() {
                    if i {
                        self.primask = disable;
                    }
                    if f && (!disable || self.execution_priority() > -1) {
                        self.faultmask = disable;
                    }
                }
            }
            Op::Svc { .. } => self.raise_sync(Exc::SVCALL, self.r[PC]),
            Op::Bkpt { .. } => {
                // No debugger: BKPT escalates to HardFault (HFSR.DEBUGEVT).
                self.diag(CoreDiag::Breakpoint { pc: self.cur_pc });
                self.ppb.hfsr |= 1 << 31;
                self.raise_sync(Exc::HARDFAULT, self.cur_pc);
            }
            Op::Udf { .. } | Op::Undefined => self.fault(Fault::UndefInstr),
        }
    }

    fn exec_dp(&mut self, op: DpOp, s: bool, rd: u8, rn: u8, op2: Operand, form: DpForm) {
        let c_in = self.flag(C);
        let (b, carry) = match op2 {
            Operand::Imm { value, carry } => (value, carry.unwrap_or(c_in)),
            Operand::Reg { rm, shift } => {
                shift_c(self.reg(rm), shift.kind, shift.amount as u32, c_in)
            }
        };
        let a = if form == DpForm::Adr {
            self.reg(15) & !3
        } else {
            self.reg(rn)
        };
        let logical = |v: u32| (v, carry, None);
        let (v, c, ovf) = match op {
            DpOp::And | DpOp::Tst => logical(a & b),
            DpOp::Eor | DpOp::Teq => logical(a ^ b),
            DpOp::Orr => logical(a | b),
            DpOp::Orn => logical(a | !b),
            DpOp::Bic => logical(a & !b),
            DpOp::Mov => logical(b),
            DpOp::Mvn => logical(!b),
            DpOp::Add | DpOp::Cmn => add_with_carry(a, b, false),
            DpOp::Adc => add_with_carry(a, b, c_in),
            DpOp::Sub | DpOp::Cmp => add_with_carry(a, !b, true),
            DpOp::Sbc => add_with_carry(a, !b, c_in),
            DpOp::Rsb => add_with_carry(!a, b, true),
        };
        if !op.is_test() {
            self.set_reg(rd, v);
        }
        if s && !(rd == 15 && !op.is_test()) {
            self.set_nz(v);
            self.set_c(c);
            if let Some(o) = ovf {
                self.set_v(o);
            }
        }
    }

    /// The address an access uses, and the write-back it asks for.
    fn address(&self, addr: Addr) -> (u32, Option<(u8, u32)>) {
        match addr {
            Addr::Imm {
                rn,
                off,
                index,
                wback,
            } => {
                let base = self.reg(rn);
                let offset = base.wrapping_add(off as u32);
                let a = if index { offset } else { base };
                (a, wback.then_some((rn, offset)))
            }
            Addr::Reg { rn, rm, shift } => (self.reg(rn).wrapping_add(self.reg(rm) << shift), None),
            Addr::Literal { off } => ((self.reg(15) & !3).wrapping_add(off as u32), None),
        }
    }

    fn read_special(&self, sysm: u8) -> u32 {
        match sysm {
            0..=7 => {
                let mut v = 0;
                if sysm & 4 == 0 {
                    v |= self.apsr & 0xF800_0000;
                }
                if sysm & 1 != 0 {
                    v |= self.ipsr as u32;
                }
                v // EPSR reads as zero
            }
            8 => self.msp(),
            9 => self.psp(),
            16 => self.primask as u32,
            17 | 18 => self.basepri as u32,
            19 => self.faultmask as u32,
            20 => self.control as u32,
            _ => 0,
        }
    }

    fn write_special(&mut self, sysm: u8, mask: u8, v: u32) {
        match sysm {
            0..=3 => {
                if mask & 2 != 0 {
                    self.apsr = v & 0xF800_0000;
                }
            }
            4..=7 => {}
            _ if !self.privileged() => {}
            8 => self.set_msp(v),
            9 => self.set_psp(v),
            16 => self.primask = v & 1 != 0,
            17 => self.basepri = v as u8 & self.config.prio_mask,
            18 => {
                let b = v as u8 & self.config.prio_mask;
                if b != 0 && (self.basepri == 0 || b < self.basepri) {
                    self.basepri = b;
                }
            }
            19 => {
                if self.execution_priority() > -1 {
                    self.faultmask = v & 1 != 0;
                }
            }
            20 => {
                // SPSEL only changes in Thread mode.
                let spsel = if self.handler_mode() {
                    self.control & 2
                } else {
                    v as u8 & 2
                };
                self.set_stack_selection(self.ipsr, spsel | (v as u8 & 1));
            }
            _ => {}
        }
    }

    fn enter_sleep(&mut self, bus: &mut impl Bus, kind: Sleep) {
        self.sleep = kind;
        self.sleeps += 1;
        bus.sleep(self.ppb.sleepdeep());
    }
}

fn size_of(w: Width) -> Size {
    match w {
        Width::Byte => Size::Byte,
        Width::Half => Size::Half,
        Width::Word => Size::Word,
    }
}

/// AddWithCarry: (result, carry out, Some(overflow)).
pub fn add_with_carry(a: u32, b: u32, c: bool) -> (u32, bool, Option<bool>) {
    let u = a as u64 + b as u64 + c as u64;
    let s = a as i32 as i64 + b as i32 as i64 + c as i64;
    let r = u as u32;
    (r, u >> 32 != 0, Some(r as i32 as i64 != s))
}

/// Shift_C, for an amount from an immediate (0..=32) or a register (0..=255).
pub fn shift_c(v: u32, kind: ShiftKind, amount: u32, c: bool) -> (u32, bool) {
    if amount == 0 && kind != ShiftKind::Rrx {
        return (v, c);
    }
    match kind {
        ShiftKind::Lsl => match amount {
            1..=31 => (v << amount, (v >> (32 - amount)) & 1 != 0),
            32 => (0, v & 1 != 0),
            _ => (0, false),
        },
        ShiftKind::Lsr => match amount {
            1..=31 => (v >> amount, (v >> (amount - 1)) & 1 != 0),
            32 => (0, v >> 31 != 0),
            _ => (0, false),
        },
        ShiftKind::Asr => {
            let a = amount.min(32);
            let r = if a == 32 {
                ((v as i32) >> 31) as u32
            } else {
                ((v as i32) >> a) as u32
            };
            let c = if a == 32 {
                v >> 31 != 0
            } else {
                (v >> (a - 1)) & 1 != 0
            };
            (r, c)
        }
        ShiftKind::Ror => {
            let r = v.rotate_right(amount % 32);
            (r, r >> 31 != 0)
        }
        ShiftKind::Rrx => ((c as u32) << 31 | v >> 1, v & 1 != 0),
    }
}
