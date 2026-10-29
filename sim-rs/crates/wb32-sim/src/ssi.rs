//! The DesignWare SSI the chip has twice: QSPI at 0x4000_3000 (IRQ 26), in
//! plain SPI mode with the board's NOR flash on it, and SPIM2 at
//! 0x4000_9000 (IRQ 27), whose MOSI drives the WS2812 chain. A port of the
//! controller half of ../../sim/peripherals/qspi.py and spim2.py (register
//! layout from wb32fq95xx.h SPI_TypeDef, behaviour from the DW SSI as
//! ChibiOS-contrib's SPIv1 driver uses it):
//!   0x00 CR0, 0x04 CR1, 0x14 BAUDR: writable only while SPIENR = 0; a write
//!        that would change one while enabled is an error and is dropped
//!        (ChibiOS rewrites CR0 unchanged while enabled).
//!   0x10 SER, 0x18 TXFTLR, 0x1C RXFTLR: writable any time; the thresholds
//!        only take values below FIFO_DEPTH, so ChibiOS's probe finds it.
//!   0x08 SPIENR: 0 flushes the RX FIFO.
//!   0x20 TXFLR always 0 (a byte written to DR is shifted at once), 0x24
//!   RXFLR, 0x28 SR (TFNF, TFE, RFNE, RFF; never BUSY), 0x2C IER, 0x30 ISR =
//!   RISR & IER, 0x34 RISR (TXE, RXO, RXF), 0x38-0x48 the clear registers
//!   (reading ICR or RXOICR clears RXO), 0x60 DR.
//! A byte beyond FIFO_DEPTH is dropped and sets RXO. The IRQ line is high
//! while ISR != 0. Not modelled: clock timing, DMA, frame formats other
//! than 8-bit Motorola SPI. FIFO_DEPTH 8 is an assumption (undocumented).
//!
//! What's on the wire is the board's: an [`SsiDevice`].

use crate::chip::Write;
use crate::gpio::Gpio;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub const FIFO_DEPTH: usize = 8;
pub const QSPI_IRQ: u32 = 26;
pub const SPIM2_IRQ: u32 = 27;

const CR0: u32 = 0x00;
const CR1: u32 = 0x04;
const SPIENR: u32 = 0x08;
const SER: u32 = 0x10;
const BAUDR: u32 = 0x14;
const TXFTLR: u32 = 0x18;
const RXFTLR: u32 = 0x1C;
const IER: u32 = 0x2C;
const DR: u32 = 0x60;

/// The controller's set-up when a byte goes out.
#[derive(Clone, Copy, Debug)]
pub struct SsiLine {
    pub cr0: u32,
    pub baudr: u32,
}

/// What hangs off the controller's pins.
pub trait SsiDevice {
    /// One byte shifted out on MOSI; returns the byte on MISO. `gpio`
    /// shows the chip's pins (a chip select on a GPIO).
    fn shift(&mut self, b: u8, line: SsiLine, gpio: &Gpio) -> u8;

    /// CR0, CR1 or BAUDR was written while disabled.
    fn configured(&mut self) {}
}

impl<T: SsiDevice> SsiDevice for Rc<RefCell<T>> {
    fn shift(&mut self, b: u8, line: SsiLine, gpio: &Gpio) -> u8 {
        self.borrow_mut().shift(b, line, gpio)
    }
    fn configured(&mut self) {
        self.borrow_mut().configured()
    }
}

pub struct Ssi {
    /// "QSPI" or "SPIM2", as the models log their writes.
    pub block: &'static str,
    irq: u32,
    cr0: u32,
    cr1: u32,
    spienr: u32,
    ser: u32,
    baudr: u32,
    txftlr: u32,
    rxftlr: u32,
    ier: u32,
    rx: VecDeque<u8>,
    rxo: bool,
    /// Firmware misuse (the models' ErrorLog lines).
    pub errors: Vec<String>,
    pub device: Option<Box<dyn SsiDevice>>,
}

impl Ssi {
    pub fn new(block: &'static str, irq: u32) -> Self {
        Ssi {
            block,
            irq,
            cr0: 0x07,
            cr1: 0,
            spienr: 0,
            ser: 0,
            baudr: 0,
            txftlr: 0,
            rxftlr: 0,
            ier: 0,
            rx: VecDeque::new(),
            rxo: false,
            errors: Vec::new(),
            device: None,
        }
    }

    /// A system reset: the registers go back; the device stays.
    pub fn reset(&mut self) {
        let device = self.device.take();
        *self = Ssi { device, ..Ssi::new(self.block, self.irq) };
    }

    fn risr(&self) -> u32 {
        // TXE: the TX level (always 0) is at or below TXFTLR.
        1 | (self.rxo as u32) << 3 | ((self.rx.len() > self.rxftlr as usize) as u32) << 4
    }

    /// Bit `irq` while ISR != 0.
    pub fn irq_level(&self) -> u64 {
        ((self.risr() & self.ier != 0) as u64) << self.irq
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            DR => self.rx.pop_front().unwrap_or(0) as u32,
            0x20 => 0,
            0x24 => self.rx.len() as u32,
            0x28 => 0x06 | (!self.rx.is_empty() as u32) << 3 | ((self.rx.len() >= FIFO_DEPTH) as u32) << 4,
            0x30 => self.risr() & self.ier,
            0x34 => self.risr(),
            0x3C | 0x48 => std::mem::take(&mut self.rxo) as u32,
            CR0 => self.cr0,
            CR1 => self.cr1,
            SPIENR => self.spienr,
            SER => self.ser,
            BAUDR => self.baudr,
            TXFTLR => self.txftlr,
            RXFTLR => self.rxftlr,
            IER => self.ier,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, gpio: &Gpio, log: &mut impl FnMut(Write)) {
        if off == DR {
            if self.spienr & 1 == 0 {
                self.errors.push(format!("{}: DR write while SPIENR = 0, ignored", self.block.to_lowercase()));
                return;
            }
            let line = SsiLine { cr0: self.cr0, baudr: self.baudr };
            let r = match &mut self.device {
                Some(d) => d.shift(v as u8, line, gpio),
                None => 0xFF,
            };
            if self.rx.len() < FIFO_DEPTH {
                self.rx.push_back(r);
            } else {
                self.rxo = true;
            }
            return;
        }
        log(Write { block: self.block, off, value: v });
        match off {
            SPIENR => {
                self.spienr = v & 1;
                if v & 1 == 0 {
                    self.rx.clear();
                }
            }
            CR0 | CR1 | BAUDR => {
                let reg = match off {
                    CR0 => &mut self.cr0,
                    CR1 => &mut self.cr1,
                    _ => &mut self.baudr,
                };
                if self.spienr & 1 == 0 {
                    *reg = v;
                    if let Some(d) = &mut self.device {
                        d.configured();
                    }
                } else if v != *reg {
                    self.errors.push(format!(
                        "{}: write to 0x{off:02x} while SPIENR = 1, ignored",
                        self.block.to_lowercase()
                    ));
                }
            }
            TXFTLR if (v as usize) < FIFO_DEPTH => self.txftlr = v,
            RXFTLR if (v as usize) < FIFO_DEPTH => self.rxftlr = v,
            SER => self.ser = v,
            IER => self.ier = v & 0x3F,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo(Vec<u8>);
    impl SsiDevice for Echo {
        fn shift(&mut self, b: u8, _: SsiLine, _: &Gpio) -> u8 {
            self.0.push(b);
            !b
        }
    }

    #[test]
    fn locked_while_enabled_fifo_overflow_and_irq() {
        let gpio = Gpio::default();
        let mut s = Ssi::new("QSPI", QSPI_IRQ);
        s.device = Some(Box::new(Echo(Vec::new())));
        let mut log = |_| {};
        s.write(DR, 1, &gpio, &mut log);
        assert_eq!(s.errors.len(), 1);
        s.write(BAUDR, 32, &gpio, &mut log);
        s.write(SPIENR, 1, &gpio, &mut log);
        s.write(BAUDR, 32, &gpio, &mut log);
        assert_eq!(s.errors.len(), 1, "an unchanged value is fine");
        s.write(BAUDR, 16, &gpio, &mut log);
        assert_eq!((s.errors.len(), s.read(BAUDR)), (2, 32));
        // The depth probe: thresholds of FIFO_DEPTH and up are dropped.
        s.write(RXFTLR, 8, &gpio, &mut log);
        s.write(RXFTLR, 7, &gpio, &mut log);
        assert_eq!(s.read(RXFTLR), 7);
        s.write(IER, 1 << 4 | 1 << 3, &gpio, &mut log);
        for b in 0..9 {
            s.write(DR, b, &gpio, &mut log);
        }
        assert_eq!(s.read(0x24), 8);
        assert_eq!(s.read(0x34), 0x19);
        assert_eq!(s.irq_level(), 1 << QSPI_IRQ);
        assert_eq!(s.read(DR), 0xFF);
        assert_eq!(s.read(0x3C), 1);
        assert_eq!(s.read(0x34), 0x01);
        assert_eq!(s.irq_level(), 0);
    }
}
