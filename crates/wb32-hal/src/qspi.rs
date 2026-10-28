//! The QSPI controller (a DesignWare SSI) as a plain SPI master, blocking.
//!
//! Set up as QMK's `spi_start` does for the external flash
//! (`platforms/chibios/drivers/spi_master.c` and ChibiOS-contrib
//! `LLD/SPIv1/hal_spi_lld.c`): SCK B3, MISO B4, MOSI B5 on AF5, mode 0,
//! 8-bit frames, MSB first, `BAUDR` = divisor. `sim/golden/qmk_qspi_init.txt`
//! holds QMK's sequence. Differences: the clock stays on and the block stays
//! enabled between transfers (QMK turns both off after each flash operation),
//! and transfers are polled, not interrupt driven.
//!
//! Chip select is a GPIO, driven by the caller.

use wb32_pac::{QSPI, RCC, spi};

use crate::gpio::{AnyPin, Mode, PB3, PB4, PB5};
use crate::rcc;

const SR_RFNE: u32 = 1 << 3;
const SR_TFNF: u32 = 1 << 1;

/// Status polls with no byte received before `transfer` gives up. A byte
/// takes about 2.7 us at 3 MHz; this is several milliseconds at 96 MHz.
const STALL_POLLS: u32 = 100_000;

/// The controller stopped returning bytes (lost clock, or a wrong FIFO depth).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stalled;

pub struct Qspi {
    /// TX FIFO depth found by `new`; frames in flight are kept below it.
    fifo: usize,
}

impl Qspi {
    /// `divisor`: SCK = PCLK1 / divisor (even, >= 2). QMK uses 32 (3 MHz).
    pub fn new(sck: PB3, miso: PB4, mosi: PB5, divisor: u16) -> Self {
        assert!(divisor >= 2 && divisor % 2 == 0);
        rcc::enable_apb1(|w| w.set_qspien(true));
        RCC.apb1rstr().modify(|w| w.set_qspirst(true));
        RCC.apb1rstr().modify(|w| w.set_qspirst(false));

        for pin in [AnyPin::from(sck), AnyPin::from(miso), AnyPin::from(mosi)] {
            pin.set_mode(Mode::alternate(5));
        }

        let r = QSPI;
        r.spienr().write_value(spi::regs::Spienr(0));
        r.baudr().write_value(spi::regs::Baudr(divisor as u32));
        r.ser().write_value(spi::regs::Ser(1));
        // 8-bit frames, Motorola SPI, mode 0, transmit and receive.
        r.cr0().write_value(spi::regs::Cr0(0x07));

        // FIFO depth probe, as ChibiOS's spi_lld_start: TXFTLR only holds
        // values below the depth.
        let mut fifo = 1;
        while fifo < 256 {
            r.txftlr().write_value(spi::regs::Txftlr(fifo as u32));
            if r.txftlr().read().0 != fifo as u32 {
                break;
            }
            fifo += 1;
        }
        r.txftlr().write_value(spi::regs::Txftlr(0));
        r.rxftlr().write_value(spi::regs::Rxftlr(0));
        r.ier().write_value(spi::regs::Ier(0));
        r.spienr().write_value(spi::regs::Spienr(1));

        Qspi { fifo: if fifo <= 1 { 1 } else { fifo } }
    }

    pub fn fifo_depth(&self) -> usize {
        self.fifo
    }

    /// Sends `tx` (or 0xFF while it runs out), storing what comes back in
    /// `rx` (when given). Shifts `n` = max(tx.len(), rx.len()) bytes.
    pub fn transfer(&mut self, tx: &[u8], mut rx: Option<&mut [u8]>) -> Result<(), Stalled> {
        let n = tx.len().max(rx.as_deref().map_or(0, |b| b.len()));
        let r = QSPI;
        let (mut sent, mut got) = (0, 0);
        let mut idle = 0u32;
        while got < n {
            // Keep fewer frames in flight than the RX FIFO holds.
            while sent < n && sent - got < self.fifo && r.sr().read().0 & SR_TFNF != 0 {
                let b = tx.get(sent).copied().unwrap_or(0xFF);
                r.dr().write_value(spi::regs::Dr(b as u32));
                sent += 1;
            }
            if r.sr().read().0 & SR_RFNE != 0 {
                let b = r.dr().read().0 as u8;
                if let Some(rx) = rx.as_deref_mut()
                    && let Some(slot) = rx.get_mut(got)
                {
                    *slot = b;
                }
                got += 1;
                idle = 0;
            } else {
                idle += 1;
                if idle >= STALL_POLLS {
                    return Err(Stalled);
                }
            }
        }
        Ok(())
    }

    pub fn write(&mut self, tx: &[u8]) -> Result<(), Stalled> {
        self.transfer(tx, None)
    }

    pub fn read(&mut self, rx: &mut [u8]) -> Result<(), Stalled> {
        self.transfer(&[], Some(rx))
    }
}
