//! Stop mode: the clocks off until an EXTI line wakes the chip. A port of
//! the vendor's `wb32_stop_mode` (QMK keyboards/linker/wireless/lpwr_wb32.c),
//! which the stock firmware runs on this board, register for register:
//!
//! 1. SysTick off; the PWR mode "STOP LP4, S32KON" (CR0 `&= 0x09; |= 0x3B004`,
//!    CFGR `0x3B3`);
//! 2. PRE_LP, the vendor's blob (disassembled): opens the ANCTL write window
//!    (ANAKEY1/2 = 3, 0x0C) and, if the low nibble of ANCTL+0x004 is above 8,
//!    writes 8 there;
//! 3. SLEEPDEEP, WFI;
//! 4. POST_LP: the window again; ANCTL+0x004 back to the trim in the info
//!    page (`*(u8 *)0x1FFF_F310 & 0xF`) if it differs; a 35-loop delay;
//! 5. SLEEPDEEP off, the clocks back up (`rcc::reinit`, as the vendor's
//!    `__early_init`), the peripheral clock enables as they were, SysTick on.
//!
//! The caller sets up the EXTI lines to wake on. Where the vendor clears
//! every pending interrupt first (or the WFI would return at once), [`enter`]
//! doesn't stop at all while one is pending, so none is lost: try again
//! later. Time (SysTick) stands still while stopped; TIM2 (the watchdog's
//! tick, `crate::tick`) stops with APB1's clock, and is paused here as well
//! (CR1.CEN), which the sim needs: Renode's timer doesn't stop.

use cortex_m::peripheral::{NVIC, SCB, SYST};
use wb32_pac::{EXTI, PWR, RCC, TIM2, pwr};

use crate::rcc;

const ANCTL_004: *mut u32 = 0x4001_0404 as *mut u32;
const TRIM: *const u8 = 0x1FFF_F310 as *const u8;

fn anctl_unlock() {
    PWR.anakey1().write_value(pwr::regs::Anakey1(0x03));
    PWR.anakey2().write_value(pwr::regs::Anakey2(0x0C));
}

fn pre_lp() {
    anctl_unlock();
    // SAFETY: ANCTL+0x004, open for writing above.
    unsafe {
        if ANCTL_004.read_volatile() & 0xF > 8 {
            ANCTL_004.write_volatile(8);
        }
    }
}

fn post_lp(settle: u32) {
    anctl_unlock();
    // SAFETY: the info page is readable; ANCTL+0x004 as above.
    unsafe {
        let trim = (TRIM.read_volatile() & 0xF) as u32;
        if ANCTL_004.read_volatile() != trim {
            ANCTL_004.write_volatile(trim);
        }
    }
    for _ in 0..settle {
        cortex_m::asm::nop();
    }
}

/// How [`enter`] stops: [`Opts::VENDOR`] is the vendor's, but for pending
/// interrupts; the rest are diagnostics, for a stop that doesn't come back.
#[derive(Clone, Copy)]
pub struct Opts {
    /// Clear pending interrupts and stop anyway, as the vendor does, rather
    /// than not stop.
    pub clear_pending: bool,
    /// POST_LP's delay loop count (the vendor's 35).
    pub settle: u32,
    /// Not the vendor's: back on the internal clock with the PLL off before
    /// the clocks are set up again (`rcc::clocks_off`).
    pub restart_clocks: bool,
}

impl Opts {
    pub const VENDOR: Opts = Opts { clear_pending: false, settle: 35, restart_clocks: false };
}

/// Reads of PLLSR out of a stop before giving up (`rcc::reinit`): well
/// over a second at the 8 MHz the chip wakes on.
const PLL_POLLS: u32 = 1_000_000;

/// Clears what would end a WFI at once, as the vendor's wb32_stop_mode:
/// every EXTI line, every NVIC line and SysTick's.
fn clear_pending() {
    EXTI.pr().write_value(wb32_pac::exti::regs::Pr(0x7FFFF));
    // SAFETY: writes to clear-pending registers only.
    let nvic = unsafe { &*NVIC::PTR };
    for i in 0..8 {
        unsafe { nvic.icpr[i].write(u32::MAX) };
    }
    SCB::clear_pendst();
}

/// True if an interrupt or exception is pending that would end a WFI at
/// once: an enabled NVIC line, SysTick, or an EXTI line that's unmasked.
fn pending() -> bool {
    // SAFETY: reads only.
    let nvic = unsafe { &*NVIC::PTR };
    let nvic_pending = (0..2).any(|i| nvic.ispr[i].read() & nvic.iser[i].read() != 0);
    nvic_pending || SCB::is_pendst_pending() || EXTI.pr().read().0 & EXTI.imr().read().0 != 0
}

/// How far a stop got, for [`enter`]'s `mark` (a diagnostic).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Out of the WFI, before the clocks are back.
    Woke,
    /// The clocks and SysTick back.
    Clocked,
}

/// Stops the chip until one of the EXTI lines the caller unmasked fires, and
/// returns the pending lines (EXTI PR) on waking; `None`, without stopping,
/// if an interrupt was already pending. Call with interrupts disabled
/// (PRIMASK): the wake doesn't run a handler, the caller clears the lines
/// it set up. `mark` is called as the wake gets past each [`Mark`].
pub fn enter(opts: Opts, mark: impl Fn(Mark)) -> Option<u32> {
    // SAFETY: the caller has interrupts off; SysTick, SCB and these
    // registers are only touched here meanwhile.
    let syst = unsafe { &*SYST::PTR };
    let scb = unsafe { &*SCB::PTR };
    const SYST_ENABLE: u32 = 1;
    const SLEEPDEEP: u32 = 1 << 2;

    unsafe { syst.csr.modify(|v| v & !SYST_ENABLE) };
    if opts.clear_pending {
        clear_pending();
    } else if pending() {
        unsafe { syst.csr.modify(|v| v | SYST_ENABLE) };
        return None;
    }
    let apb1 = RCC.apb1enr().read();
    let apb2 = RCC.apb2enr().read();
    let ahb = (RCC.ahbenr0().read(), RCC.ahbenr1().read(), RCC.ahbenr2().read());
    let tim2 = TIM2.cr1().read();
    TIM2.cr1().modify(|w| w.set_cen(false));

    PWR.cr0().modify(|w| w.0 &= 0x09);
    PWR.cr0().modify(|w| w.0 |= 0x3B004);
    PWR.cfgr().write_value(pwr::regs::Cfgr(0x3B3));
    pre_lp();
    unsafe { scb.scr.modify(|v| v | SLEEPDEEP) };
    cortex_m::asm::dsb();
    cortex_m::asm::wfi();
    mark(Mark::Woke);
    post_lp(opts.settle);
    unsafe { scb.scr.modify(|v| v & !SLEEPDEEP) };
    let fired = EXTI.pr().read().0;

    if opts.restart_clocks {
        rcc::clocks_off();
    }
    rcc::reinit(PLL_POLLS);
    RCC.apb1enr().write_value(apb1);
    RCC.apb2enr().write_value(apb2);
    RCC.ahbenr0().write_value(ahb.0);
    RCC.ahbenr1().write_value(ahb.1);
    RCC.ahbenr2().write_value(ahb.2);
    TIM2.cr1().write_value(tim2);
    unsafe { syst.csr.modify(|v| v | SYST_ENABLE) };
    mark(Mark::Clocked);
    Some(fired)
}
