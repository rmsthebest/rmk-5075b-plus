//! Start-up: clocks, the optional watchdog, and the delay. ROM
//! 0x1FFFE940-0x1FFFEAD5 (the first part of main), 0x1FFFE178 and
//! 0x1FFFE904.

use crate::mmio::*;
use wb32_pac::{ANCTL, CACHE, GPIOD, IWDG, PWR, RCC};

/// ROM 0x1FFFE940-0x1FFFEAD5: from reset to the 48 MHz PLL.
pub fn start() {
    cpsid();
    // Priority grouping 3: CMSIS's NVIC_SetPriorityGrouping(3).
    let aircr = r32(SCB_AIRCR);
    w32(SCB_AIRCR, (aircr & 0xF8FF) | 0x300 | 0x05FA_0000);
    // Unlock the analog registers.
    w32(PWR.anakey1(), 0x03);
    w32(PWR.anakey2(), 0x0C);
    // Run from MHSI (8 MHz) while the clocks are set up.
    w32(ANCTL.mhsienr(), 1);
    while r32(ANCTL.mhsisr()) == 0 {}
    w32(RCC.mainclksrc(), 0);
    w32(RCC.mainclkuen(), 1);
    w32(CACHE.cr(), 0x0300_0011);

    if r32(RCC_IWDG_ON) != 0 {
        iwdg_start();
    }

    w32(ANCTL.porcr(), 0x7BE);
    w32(ANCTL.fhsienr(), 1);
    w32(SCB_VTOR, 0x1FFF_E000);
    w32(SYST_CSR, 0);
    w32(NVIC_ICER0, 0xFFFF_FFFF);
    w32(NVIC_ICER1, 0xFFFF_FFFF);
    // PENDSVCLR | PENDSTCLR.
    w32(SCB_ICSR, 0x0A00_0000);
    cpsie();

    // A factory value in information block 2: 0x000B at 0x1FFFF24C, or its
    // complement in the next halfword. Without it the ROM stops here, before
    // it touches USB. The chip has 0x000B.
    if r16(reg(INFO + 0x24C)) != 0x000B && !r16(reg(INFO + 0x24E)) != 0x000B {
        loop {}
    }

    w32(RCC.ahbpre(), 0);
    w32(RCC.apb1pre(), 0x80);
    // Written again with what it reads back, as the ROM does.
    m32(RCC.apb1pre(), |v| v);
    // APB1: clocks on, the 0x1E0 group pulsed through reset, then on.
    w32(RCC.apb1enr(), 0x81E0);
    m32(RCC.apb1rstr(), |v| v | 0x1E0);
    m32(RCC.apb1rstr(), |v| v & !0x1E0);
    w32(RCC.apb1enr(), 0x8120);
    // PD0/PD1 (OSC_IN/OSC_OUT) to analog for the crystal.
    w32(GPIOD.cfgmsk(), 0xFFFC);
    w32(GPIOD.moder(), 0x0F);
    // HSE on. No crystal and it waits here for ever.
    w32(ANCTL.hsecr1(), 2);
    w32(ANCTL.hsecr0(), 1);
    while r32(ANCTL.hsesr()) == 0 {}
    w32(CACHE.cr(), 0x0300_0011);
    // PLL from HSE: PLLSRC 1, prescaler 0x25 built up in three writes.
    w32(RCC.pllsrc(), 1);
    w32(RCC.pllpre(), 0x20);
    m32(RCC.pllpre(), |v| v | 0x04);
    m32(RCC.pllpre(), |v| v | 0x01);
    w32(ANCTL.pllcr(), 0x80);
    w32(ANCTL.pllenr(), 1);
    while r32(ANCTL.pllsr()) != 3 {}
    // Main clock to the PLL (48 MHz: the delays below assume it).
    w32(RCC.mainclksrc(), 2);
    w32(RCC.mainclkuen(), 1);
    w32(PWR.anakey1(), 0);
}

/// ROM 0x1FFFE98E-0x1FFFE9F7: the IWDG, when RCC+0x104 asks for it:
/// prescaler 6, reload 0xFFF (about 26 s on the LSI). There is no 0xCCCC
/// start key; RCC+0x104 presumably reflects a watchdog that's already
/// running. The 0xAAAA before the first 0x5555 and the second 0x5555 are
/// the ROM's.
fn iwdg_start() {
    w32(SYST_CSR, 0);
    w32(SYST_RVR, 0x6E0);
    w32(SYST_CVR, 0);
    w32(SYST_CSR, 5);
    while r32(SYST_CSR) & (1 << 16) == 0 {}
    w32(SYST_CSR, 0);
    m32(RCC.ahbenr0(), |v| v | 0x04);
    while r32(IWDG.sr()) & 0x02 != 0 {}
    w32(IWDG.kr(), 0xAAAA);
    while r32(IWDG.sr()) & 0x02 != 0 {}
    w32(IWDG.kr(), 0x5555);
    w32(IWDG.pr(), 6);
    while r32(IWDG.sr()) & 0x01 != 0 {}
    w32(IWDG.kr(), 0x5555);
    w32(IWDG.rlr(), 0xFFF);
    while r32(IWDG.sr()) & 0x02 != 0 {}
}

/// ROM 0x1FFFE904-0x1FFFE91F: feeds the IWDG when it's on and not busy.
/// Every wait loop calls it.
pub fn feed() {
    if r32(RCC_IWDG_ON) != 0 && r32(IWDG.sr()) & 0x02 == 0 {
        w32(IWDG.kr(), 0xAAAA);
    }
}

/// ROM 0x1FFFE178-0x1FFFE19F: one SysTick period of `ticks` core clocks
/// (48 000 is 1 ms), feeding the watchdog while it waits. It also stops if
/// SysTick was turned off under it.
pub fn delay(ticks: u32) {
    w32(SYST_RVR, ticks);
    w32(SYST_CVR, 0);
    w32(SYST_CSR, 5);
    loop {
        let csr = r32(SYST_CSR);
        feed();
        if csr & 1 == 0 || csr & (1 << 16) != 0 {
            break;
        }
    }
    m32(SYST_CSR, |v| v & !1);
}
