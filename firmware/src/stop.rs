//! Stop mode while asleep (M7c): the wake lines around `wb32_hal::stop`,
//! as the vendor's `lpwr_exti_init` and its board hook (wls.c):
//! - the rows, A0-A4 and C13 (EXTI 0-4, 13), falling: the scan has every
//!   column driven low while asleep, so a key pulls its row low;
//! - the knob's B6 (EXTI 6), both edges: unlike the vendor's, the knob wakes
//!   it, as in light sleep;
//! - the cable, A7 (EXTI 7), rising. Line 7 is the knob's B7 otherwise: it's
//!   A7's while stopped and B7's again after;
//! - the radio module's UART RX, C11 (EXTI 11), falling (a start bit), as an
//!   input, as the vendor's. That frame is lost (UART3 has no clock until
//!   the PLL is back, over 2 ms); the module sends it again.
//!
//! In lower sleep (the flat battery) only the cable and the UART wake it.
//!
//! With `diag`, the Caps and Win-lock LEDs (both active low) show how far
//! the last stop got: Caps alone, it's stopped or never woke; Win-lock
//! alone, it got out with its clocks back (until the firmware sets them
//! again). A wake whose PLL doesn't lock panics in `rcc::reinit`, with the
//! reason kept for Vial 0xD2. (LEDs written between the wake and the
//! clocks would go nowhere: GPIO has no clock then.) `diag`'s variant bits
//! (`VARIANT_*`) try what the vendor does otherwise.
//!
//! The EXTI setup is put back as it was after; a knob or row edge that woke
//! it is left pending for the knob's or the parked scan's `ExtiInput` (if
//! armed). The turn
//! that wakes it is mostly lost all the same (B7's edge isn't seen while
//! line 7 is A7's); the next one is read.

use wb32_hal::gpio::{AnyPin, Mode, Port, Pull};
use wb32_pac::{AFIO, EXTI, GPIOA, GPIOB, GPIOC, Interrupt};

pub const ROW_LINES: u32 = 0x1F | 1 << 13;
pub const KNOB_LINE: u32 = 1 << 6;
pub const CABLE_LINE: u32 = 1 << 7;
pub const UART_LINE: u32 = 1 << 11;

/// Each wake line's port (AFIO EXTICR: 0 A, 1 B, 2 C).
const PORTS: [(u8, Port); 9] = [
    (0, Port::A),
    (1, Port::A),
    (2, Port::A),
    (3, Port::A),
    (4, Port::A),
    (6, Port::B),
    (7, Port::A),
    (11, Port::C),
    (13, Port::C),
];

/// The lines behind each of `IRQS`.
const IRQ_LINES: [u32; 7] = [1 << 0, 1 << 1, 1 << 2, 1 << 3, 1 << 4, 0x3E0, 0xFC00];

fn irq_index(irq: Interrupt) -> usize {
    IRQS.iter().position(|&i| i == irq).unwrap_or(0)
}

const IRQS: [Interrupt; 7] = [
    Interrupt::EXTI0,
    Interrupt::EXTI1,
    Interrupt::EXTI2,
    Interrupt::EXTI3,
    Interrupt::EXTI4,
    Interrupt::EXTI9_5,
    Interrupt::EXTI15_10,
];

/// True while no key is down: every row high (with the columns driven low,
/// a held key would keep its row low and never make the falling edge).
pub fn rows_high() -> bool {
    GPIOA.idr().read().0 & 0x1F == 0x1F && GPIOC.idr().read().0 & 1 << 13 != 0
}

/// Diagnostic variants (`diag`'s bits), each as the vendor's does it.
/// The rows and the UART wake on both edges.
pub const VARIANT_BOTH_EDGES: u8 = 1 << 0;
/// Pending interrupts are cleared and it stops anyway.
pub const VARIANT_CLEAR_PENDING: u8 = 1 << 1;
/// The knob doesn't wake it.
pub const VARIANT_NO_KNOB: u8 = 1 << 2;
/// Not the vendor's: the lines are events too (EXTI EMR).
pub const VARIANT_EVENTS: u8 = 1 << 3;
/// Not the vendor's: 100 times POST_LP's delay.
pub const VARIANT_SLOW_SETTLE: u8 = 1 << 4;
pub const VARIANTS: u8 = (1 << 5) - 1;

const CAPS_LED: u32 = 1 << 5;
const WIN_LOCK_LED: u32 = 1 << 0;

/// The diagnostic LEDs: `on` low (BSRR's upper half), the rest high.
fn diag_leds(caps: bool, win_lock: bool) {
    let bsrr = |on: bool, pin: u32| if on { pin << 16 } else { pin };
    GPIOC
        .bsrr()
        .write_value(wb32_pac::gpio::regs::Bsrr(bsrr(caps, CAPS_LED)));
    GPIOB
        .bsrr()
        .write_value(wb32_pac::gpio::regs::Bsrr(bsrr(win_lock, WIN_LOCK_LED)));
}

/// Stops until a wake line fires; returns those that fired, or None if it
/// didn't stop (an interrupt was pending: try again later). With `diag`
/// (its variant bits), the LEDs mark its progress (see the top of this
/// file).
pub fn stop(lower: bool, diag: Option<u8>) -> Option<u32> {
    let v = diag.unwrap_or(0);
    let knob = if v & VARIANT_NO_KNOB != 0 {
        0
    } else {
        KNOB_LINE
    };
    let lines = CABLE_LINE | UART_LINE | if lower { 0 } else { ROW_LINES | knob };
    let both = if v & VARIANT_BOTH_EDGES != 0 {
        ROW_LINES | UART_LINE
    } else {
        0
    };
    let rising = CABLE_LINE | KNOB_LINE | both;
    let falling = ROW_LINES | KNOB_LINE | UART_LINE;
    let opts = wb32_hal::stop::Opts {
        clear_pending: v & VARIANT_CLEAR_PENDING != 0,
        settle: wb32_hal::stop::Opts::VENDOR.settle
            * if v & VARIANT_SLOW_SETTLE != 0 { 100 } else { 1 },
    };
    let emr_was = EXTI.emr().read().0;
    // SAFETY: C11 is UART3's RX; it's an input only while stopped.
    let rx = unsafe { AnyPin::steal(Port::C, 11) };
    critical_section::with(|cs| {
        let imr = EXTI.imr().read().0;
        // An event for an ExtiInput not yet handled: let it be.
        if EXTI.pr().read().0 & imr != 0 {
            return None;
        }
        let rtsr = EXTI.rtsr().read().0;
        let ftsr = EXTI.ftsr().read().0;
        let exticr: [u32; 4] = core::array::from_fn(|i| AFIO.exticr(i).read().0);
        let nvic_was: [bool; 7] =
            core::array::from_fn(|i| cortex_m::peripheral::NVIC::is_enabled(IRQS[i]));

        for (line, port) in PORTS {
            if lines & 1 << line != 0 {
                let line = line as usize;
                AFIO.exticr(line / 4)
                    .modify(|w| w.set_exti(line % 4, port as u8));
            }
        }
        EXTI.imr().write_value(wb32_pac::exti::regs::Imr(0));
        EXTI.rtsr()
            .write_value(wb32_pac::exti::regs::Rtsr(rtsr & !lines | rising & lines));
        EXTI.ftsr()
            .write_value(wb32_pac::exti::regs::Ftsr(ftsr & !lines | falling & lines));
        rx.set_mode(Mode::input(Pull::None));
        EXTI.pr().write_value(wb32_pac::exti::regs::Pr(lines));
        EXTI.imr().write_value(wb32_pac::exti::regs::Imr(lines));
        for irq in IRQS {
            cortex_m::peripheral::NVIC::unpend(irq);
            // SAFETY: interrupts are off; restored below.
            unsafe { cortex_m::peripheral::NVIC::unmask(irq) };
        }

        if v & VARIANT_EVENTS != 0 {
            EXTI.emr().write_value(wb32_pac::exti::regs::Emr(lines));
        }
        if diag.is_some() {
            diag_leds(true, false);
        }
        let woke = wb32_hal::stop::enter(cs, opts);
        if diag.is_some() {
            diag_leds(false, true);
        }
        EXTI.emr().write_value(wb32_pac::exti::regs::Emr(emr_was));

        rx.set_mode(Mode::alternate(7));
        let fired = woke.unwrap_or(0) & lines;
        // The knob's or a row's edge stays pending for its ExtiInput, if it
        // waits.
        let keep = fired & imr & (KNOB_LINE | ROW_LINES);
        EXTI.imr().write_value(wb32_pac::exti::regs::Imr(0));
        for (i, v) in exticr.iter().enumerate() {
            AFIO.exticr(i).write_value(wb32_pac::afio::regs::Exticr(*v));
        }
        EXTI.rtsr().write_value(wb32_pac::exti::regs::Rtsr(rtsr));
        EXTI.ftsr().write_value(wb32_pac::exti::regs::Ftsr(ftsr));
        EXTI.pr()
            .write_value(wb32_pac::exti::regs::Pr(lines & !keep));
        EXTI.imr().write_value(wb32_pac::exti::regs::Imr(imr));
        for (irq, was) in IRQS.into_iter().zip(nvic_was) {
            if keep & IRQ_LINES[irq_index(irq)] == 0 {
                cortex_m::peripheral::NVIC::unpend(irq);
            }
            if !was {
                cortex_m::peripheral::NVIC::mask(irq);
            }
        }
        woke.map(|_| fired)
    })
}
