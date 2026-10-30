//! Exception entry and return, faults, and waking from sleep (ARM DDI 0403E
//! B1.5). Tail-chaining and late arrival aren't modelled as such: a return
//! unstacks, and the next step takes whatever is pending, which ends in the
//! same state.

use crate::CoreDiag;
use crate::bus::{Bus, Size};
use crate::cpu::{Cpu, LR, PC, SP, Sleep};

/// Exception numbers.
pub struct Exc;

impl Exc {
    pub const RESET: u16 = 1;
    pub const NMI: u16 = 2;
    pub const HARDFAULT: u16 = 3;
    pub const MEMMANAGE: u16 = 4;
    pub const BUSFAULT: u16 = 5;
    pub const USAGEFAULT: u16 = 6;
    pub const SVCALL: u16 = 11;
    pub const DEBUGMON: u16 = 12;
    pub const PENDSV: u16 = 14;
    pub const SYSTICK: u16 = 15;

    pub const fn irq(n: u16) -> u16 {
        16 + n
    }
}

/// Synchronous faults, with their CFSR bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    UndefInstr,
    InvState,
    InvPc,
    NoCp,
    Unaligned,
    DivByZero,
    /// An instruction fetch from where there's nothing to execute.
    IBusErr,
}

impl Fault {
    fn cfsr_bit(self) -> u32 {
        match self {
            Fault::UndefInstr => 1 << 16,
            Fault::InvState => 1 << 17,
            Fault::InvPc => 1 << 18,
            Fault::NoCp => 1 << 19,
            Fault::Unaligned => 1 << 24,
            Fault::DivByZero => 1 << 25,
            Fault::IBusErr => 1 << 8,
        }
    }

    fn exception(self) -> u16 {
        match self {
            Fault::IBusErr => Exc::BUSFAULT,
            _ => Exc::USAGEFAULT,
        }
    }
}

impl Cpu {
    /// The priority the core runs at: the highest active exception's group
    /// priority, boosted by BASEPRI, PRIMASK and FAULTMASK. 256 when
    /// nothing is active or masked.
    pub fn execution_priority(&self) -> i16 {
        self.priority_without_primask()
            .min(if self.primask { 0 } else { 256 })
    }

    fn priority_without_primask(&self) -> i16 {
        let mut p = 256;
        let mut set = self.ppb.active;
        while set != 0 {
            let exc = set.trailing_zeros() as u16;
            set &= set - 1;
            p = p.min(self.ppb.group(self.ppb.priority_of(exc)));
        }
        if self.basepri != 0 {
            p = p.min(self.ppb.group(self.basepri as i16));
        }
        if self.faultmask {
            p = p.min(-1);
        }
        p
    }

    pub(crate) fn update_irqs(&mut self, bus: &mut impl Bus) {
        let lines = bus.irq_lines();
        self.ppb.irq_inputs(lines.level, lines.raised);
        if std::mem::take(&mut self.ppb.sev) {
            self.event = true;
        }
    }

    /// Takes the highest pending exception if it preempts. True if it did.
    pub(crate) fn take_pending(&mut self, bus: &mut impl Bus) -> bool {
        match self.ppb.highest_pending() {
            Some((exc, group)) if group < self.execution_priority() => {
                let ret = self.r[PC];
                self.enter(bus, exc, ret);
                true
            }
            _ => false,
        }
    }

    /// Whether a sleeping core wakes: an exception that would preempt with
    /// PRIMASK ignored (WFI and WFE), or the event register (WFE).
    pub(crate) fn should_wake(&mut self) -> bool {
        let preempts = matches!(self.ppb.highest_pending(), Some((_, g)) if g < self.priority_without_primask());
        match self.sleep {
            Sleep::Awake => true,
            Sleep::Wfi => preempts,
            Sleep::Wfe => {
                if preempts {
                    true
                } else if self.event {
                    self.event = false;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// A fault from the current instruction: it runs again after the handler.
    pub(crate) fn fault(&mut self, kind: Fault) {
        self.it = self.cur_it;
        self.ppb.cfsr |= kind.cfsr_bit();
        self.diag(CoreDiag::Fault {
            kind,
            pc: self.cur_pc,
        });
        self.pending_sync = Some((kind.exception(), self.cur_pc));
    }

    /// A synchronous exception (SVC, a fault, BKPT), returning to `ret`.
    /// Taken at once if it can preempt, else escalated to HardFault; a
    /// HardFault that can't be taken is a lockup.
    pub(crate) fn raise_sync(&mut self, exc: u16, ret: u32) {
        self.pending_sync = Some((exc, ret));
    }

    /// Enters a pending synchronous exception (after the instruction's
    /// state updates, so it stacks the right registers).
    pub(crate) fn take_sync(&mut self, bus: &mut impl Bus) {
        let Some((exc, ret)) = self.pending_sync.take() else {
            return;
        };
        let enabled = match exc {
            Exc::MEMMANAGE => self.ppb.shcsr & (1 << 16) != 0,
            Exc::BUSFAULT => self.ppb.shcsr & (1 << 17) != 0,
            Exc::USAGEFAULT => self.ppb.shcsr & (1 << 18) != 0,
            _ => true,
        };
        let current = self.execution_priority();
        let group = self.ppb.group(self.ppb.priority_of(exc));
        if enabled && group < current {
            self.enter(bus, exc, ret);
        } else if exc != Exc::HARDFAULT && current > -1 {
            self.ppb.hfsr |= 1 << 30; // FORCED
            self.enter(bus, Exc::HARDFAULT, ret);
        } else {
            self.diag(CoreDiag::Lockup { pc: self.cur_pc });
            self.halted = true;
        }
    }

    /// Exception entry: stack the frame, then run the handler.
    pub(crate) fn enter(&mut self, bus: &mut impl Bus, exc: u16, ret: u32) {
        // A tail-chain's cost went with the return (cpu.rs).
        if !std::mem::take(&mut self.tail_chain) {
            self.spend(crate::timing::ENTRY);
        }
        self.last_load = None;
        let sp = self.r[SP];
        let realign = self.ppb.stkalign() && sp & 4 != 0;
        let frame = sp.wrapping_sub(0x20) & !(if self.ppb.stkalign() { 4 } else { 0 });
        let xpsr = self.xpsr() | (realign as u32) << 9;
        let regs = [
            self.r[0],
            self.r[1],
            self.r[2],
            self.r[3],
            self.r[12],
            self.r[LR],
            ret & !1,
            xpsr,
        ];
        for (i, v) in regs.into_iter().enumerate() {
            self.write(bus, frame + 4 * i as u32, Size::Word, v);
        }
        self.r[SP] = frame;
        self.r[LR] = if self.handler_mode() {
            0xFFFF_FFF1
        } else if self.psp_active() {
            0xFFFF_FFFD
        } else {
            0xFFFF_FFF9
        };
        let vector = self.read(bus, self.ppb.vtor.wrapping_add(4 * exc as u32), Size::Word);
        self.set_stack_selection(exc, self.control & !2);
        self.thumb = vector & 1 != 0;
        self.r[PC] = vector & !1;
        self.it = Default::default();
        self.ppb.active |= 1 << exc;
        self.ppb.pending &= !(1 << exc);
        self.monitor = None;
        self.event = true;
        self.sleep = Sleep::Awake;
    }

    /// EXC_RETURN loaded into the PC in Handler mode.
    pub(crate) fn exception_return(&mut self, bus: &mut impl Bus, exc_return: u32) {
        let returning = self.ipsr;
        let (to_thread, use_psp) = match exc_return & 0x0FFF_FFFF {
            0x0FFF_FFF1 => (false, false),
            0x0FFF_FFF9 => (true, false),
            0x0FFF_FFFD => (true, true),
            _ => {
                self.fault(Fault::InvPc);
                return;
            }
        };
        if self.ppb.active & (1 << returning) == 0 {
            self.fault(Fault::InvPc);
            return;
        }
        self.ppb.active &= !(1 << returning);
        if returning != Exc::NMI {
            self.faultmask = false;
        }
        // Pick the stack the frame is on, then unstack.
        let spsel = if use_psp { 2 } else { 0 };
        self.set_stack_selection(
            if to_thread { 0 } else { returning },
            (self.control & !2) | spsel,
        );
        let frame = self.r[SP];
        let mut w = [0u32; 8];
        for (i, v) in w.iter_mut().enumerate() {
            *v = self.read(bus, frame + 4 * i as u32, Size::Word);
        }
        let xpsr = w[7];
        [
            self.r[0], self.r[1], self.r[2], self.r[3], self.r[12], self.r[LR],
        ] = [w[0], w[1], w[2], w[3], w[4], w[5]];
        self.r[PC] = w[6] & !1;
        let realign = self.ppb.stkalign() && xpsr & (1 << 9) != 0;
        self.r[SP] = frame.wrapping_add(0x20) | if realign { 4 } else { 0 };
        self.apsr = xpsr & 0xF800_0000;
        self.thumb = xpsr & (1 << 24) != 0;
        self.it.0 = (((xpsr >> 25) & 3) | ((xpsr >> 10) & 0x3F) << 2) as u8;
        let ipsr = (xpsr & 0x1FF) as u16;
        if to_thread != (ipsr == 0) {
            self.diag(CoreDiag::Fault {
                kind: Fault::InvPc,
                pc: self.cur_pc,
            });
        }
        self.set_stack_selection(ipsr, self.control);
        self.monitor = None;
        self.event = true;
        self.returned = true;
        if to_thread && self.ppb.sleeponexit() {
            self.sleep = Sleep::Wfi;
            self.sleeps += 1;
            bus.sleep(self.ppb.sleepdeep());
        }
    }
}
