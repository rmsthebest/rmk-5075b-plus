#![no_std]

pub use wb32_pac as pac;

pub mod exti;
pub mod gpio;
pub mod qspi;
pub mod rcc;
pub mod spi_nor;
pub mod stop;
pub mod tick;
pub mod time_driver;
pub mod uart;
pub mod usb;
pub mod ws2812;

/// Sets `irq`'s priority and unmasks it.
pub(crate) fn unmask_irq(irq: pac::Interrupt, prio: u8) {
    // SAFETY: the NVIC priority register is per interrupt, and only the
    // driver that owns `irq` sets it, once, at init.
    unsafe {
        let mut nvic = cortex_m::Peripherals::steal().NVIC;
        nvic.set_priority(irq, prio);
        cortex_m::peripheral::NVIC::unmask(irq);
    }
}

/// As [`unmask_irq`], first dropping a request left pending from before the
/// driver was set up. Not for the shared EXTI vectors, where it could drop
/// another line's edge.
pub(crate) fn enable_irq(irq: pac::Interrupt, prio: u8) {
    cortex_m::peripheral::NVIC::unpend(irq);
    unmask_irq(irq, prio);
}

/// The TX FIFO depth of a DesignWare SSI (SPIM2, QSPI), probed as ChibiOS's
/// spi_lld_start does: TXFTLR only holds values below the depth. 1 if the
/// probe finds nothing; leaves TXFTLR at 0.
pub(crate) fn ssi_fifo_depth(r: pac::spi::Spi) -> usize {
    let mut fifo = 1;
    while fifo < 256 {
        r.txftlr().write_value(pac::spi::regs::Txftlr(fifo));
        if r.txftlr().read().0 != fifo {
            break;
        }
        fifo += 1;
    }
    r.txftlr().write_value(pac::spi::regs::Txftlr(0));
    fifo as usize
}
