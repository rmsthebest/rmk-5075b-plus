//! Clock tree: 12 MHz HSE -> PLL (/2, x16) -> 96 MHz MAINCLK = HCLK = PCLK1 = PCLK2.
//!
//! [`init`] is a port of ChibiOS-contrib's `wb32_clock_init()` / `SetSysClock()`
//! (`os/hal/ports/WB32/WB32FQ95xx/hal_lld.c`) with QMK's settings for this
//! board (`mcuconf.h`: HSE on, PLLDIV 2, PLLMUL 16, all bus prescalers 1).
//! It performs the same register writes in the same order, including the
//! read-modify-writes that change nothing, so the simulator can diff it
//! against QMK's sequence (`sim-rs/golden/qmk/qmk_sysctl_writes.txt`).

use wb32_pac::{ANCTL, CACHE, GPIOD, PWR, RCC, anctl, gpio, pwr, rcc};

pub const HSE_HZ: u32 = 12_000_000;
pub const SYSCLK_HZ: u32 = 96_000_000;
pub const HCLK_HZ: u32 = SYSCLK_HZ;
pub const PCLK1_HZ: u32 = SYSCLK_HZ;
pub const PCLK2_HZ: u32 = SYSCLK_HZ;

/// Polls of HSESR before giving up (ChibiOS `HSE_STARTUP_TIMEOUT`).
const HSE_STARTUP_TIMEOUT: u32 = 48_000;

/// PLLCR.PLLMUL encoding: 0 = x24, 1 = x20, 2 = x16, 3 = x12.
const PLLMUL_X16: u8 = 2;
/// RCC.MAINCLKSRC / RCC.PLLSRC values.
const MAINCLKSRC_MHSI: u32 = 0;
const MAINCLKSRC_PLL: u32 = 2;
const PLLSRC_HSE: u32 = 1;

/// Opens the ANCTL write window (PWR ANAKEY1/2). Writes to ANCTL are ignored
/// by the hardware while it is closed.
fn anctl_unlock() {
    PWR.anakey1().write_value(pwr::regs::Anakey1(0x03));
    PWR.anakey2().write_value(pwr::regs::Anakey2(0x0C));
}

fn anctl_lock() {
    PWR.anakey1().write_value(pwr::regs::Anakey1(0x00));
    PWR.anakey2().write_value(pwr::regs::Anakey2(0x00));
}

/// Switches the chip from the 8 MHz reset clock to 96 MHz from the crystal.
/// Call once, first thing in `main`. Panics (and so resets into DFU) if the
/// crystal doesn't start, where ChibiOS would hang.
pub fn init() {
    setup(None);
}

/// [`init`] again, out of stop mode: the same, but a PLL that doesn't lock
/// in `pll_polls` reads of PLLSR panics (a reset, with the reason kept for
/// Vial 0xD2) where `init` would wait for ever, as ChibiOS does.
pub fn reinit(pll_polls: u32) {
    setup(Some(pll_polls));
}

/// Back on the 8 MHz internal clock with the PLL off (a diagnostic, before
/// [`reinit`]: a PLL left on through a stop gets a fresh start).
pub fn clocks_off() {
    anctl_unlock();
    RCC.mainclksrc()
        .write_value(rcc::regs::Mainclksrc(MAINCLKSRC_MHSI));
    RCC.mainclkuen().write_value(rcc::regs::Mainclkuen(1));
    ANCTL.pllenr().write(|w| w.set_pllon(false));
    for _ in 0..10_000 {
        if ANCTL.pllsr().read().0 & 0x03 == 0 {
            break;
        }
    }
    anctl_lock();
}

fn setup(pll_polls: Option<u32>) {
    // Turn off the power-on-reset circuit, as ChibiOS does.
    anctl_unlock();
    ANCTL.porcr().write_value(anctl::regs::Porcr(0x7BE));
    anctl_lock();

    anctl_unlock();

    // APB1CLK = MAINCLK / 1. The second write is ChibiOS's `|= 0x00`.
    RCC.apb1pre().write(|w| w.set_srcen(true));
    RCC.apb1pre().modify(|_| {});

    // PD0/PD1 are the crystal pins: hand them to the oscillator (analog mode).
    RCC.apb1enr().write(|w| {
        w.set_bmx1en(true);
        w.set_gpioden(true);
    });
    GPIOD.cfgmsk().write_value(gpio::regs::Cfgmsk(0xFFFC));
    GPIOD.moder().write_value(gpio::regs::Moder(0x0F));
    ANCTL.hsecr1().write(|w| w.set_padoen(true));
    ANCTL.hsecr0().write(|w| w.set_hseon(true));
    let hse_ready = (0..HSE_STARTUP_TIMEOUT).any(|_| ANCTL.hsesr().read().hserdy());
    assert!(hse_ready, "HSE did not start");

    // 96 MHz from flash: cache and prefetch on, 3 wait states.
    CACHE.cr().write(|w| {
        w.set_cheen(3);
        w.set_prefen(1); // PREFEN_ON
        w.set_latency(3);
    });

    // AHBCLK = MAINCLK / 1, APB2CLK = MAINCLK / 1.
    RCC.ahbpre().write_value(rcc::regs::Ahbpre(0));
    RCC.apb2pre().write(|w| w.set_srcen(true));
    RCC.apb2pre().modify(|_| {});

    // PLL: HSE / 2 * 16 = 96 MHz. PLLPRE = SRCEN, then `|= (2 - 2)`, then
    // `|= DIVEN` (ratio 0 with the divider enabled = /2).
    RCC.pllsrc().write_value(rcc::regs::Pllsrc(PLLSRC_HSE));
    RCC.pllpre().write(|w| w.set_srcen(true));
    RCC.pllpre().modify(|_| {});
    RCC.pllpre().modify(|w| w.set_diven(true));
    ANCTL.pllcr().write(|w| w.set_pllmul(PLLMUL_X16));
    ANCTL.pllenr().write(|w| w.set_pllon(true));
    let mut polls = 0u32;
    while ANCTL.pllsr().read().0 != 0x03 {
        if let Some(max) = pll_polls {
            polls += 1;
            if polls >= max {
                panic!(
                    "stop: PLL didn't lock, PLLSR {:#x} HSESR {:#x}",
                    ANCTL.pllsr().read().0,
                    ANCTL.hsesr().read().0
                );
            }
        }
    }

    RCC.mainclksrc()
        .write_value(rcc::regs::Mainclksrc(MAINCLKSRC_PLL));
    RCC.mainclkuen().write_value(rcc::regs::Mainclkuen(1));

    anctl_lock();

    // Bus matrix clocks (ChibiOS rccEnableAPB1/APB2).
    RCC.apb1enr().modify(|w| w.set_bmx1en(true));
    RCC.apb2enr().modify(|w| w.set_bmx2en(true));
}

/// Read-modify-write of RCC APB1ENR (ChibiOS `rccEnableAPB1`), written only
/// when it changes: `gpio::set_mode` calls this for every column the scan
/// switches. In a critical section, so it is safe from interrupts too.
pub fn enable_apb1(f: impl FnOnce(&mut rcc::regs::Apb1enr)) {
    critical_section::with(|_| {
        let was = RCC.apb1enr().read();
        let mut v = was;
        f(&mut v);
        if v.0 != was.0 {
            RCC.apb1enr().write_value(v);
        }
    });
}
