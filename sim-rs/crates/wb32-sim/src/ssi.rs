//! The DesignWare SSI the chip has twice: QSPI at 0x4000_3000 (IRQ 26, on
//! APB1), in plain SPI mode with the board's NOR flash on it, and SPIM2 at
//! 0x4000_9000 (IRQ 27, on APB2), whose MOSI drives the WS2812 chain. The
//! registers are RM2905025 22.10 (SPIM2) and 23.8 (QSPI), the behaviour the
//! DW SSI's as ChibiOS-contrib's SPIv1 driver uses it:
//!   0x00 CR0 (reset 0x0100_0007), 0x04 CR1, 0x14 BAUDR: writable only
//!        while SPIENR = 0; a write that would change one while enabled is
//!        an error and is dropped (ChibiOS rewrites CR0 unchanged while
//!        enabled).
//!   0x10 SER, 0x18 TXFTLR, 0x1C RXFTLR: writable any time; the thresholds
//!        only take values below the FIFO depth, so ChibiOS's probe finds it.
//!   0x08 SPIENR: 0 flushes both FIFOs and stops the frame being shifted.
//!   0x20 TXFLR, 0x24 RXFLR, 0x28 SR (BUSY, TFNF, TFE, RFNE, RFF), 0x2C IER
//!   (reset 0x1F), 0x30 ISR = RISR & IER, 0x34 RISR (TXE, TXO, RXU, RXO,
//!   RXF), 0x38-0x48 the clear registers (read to clear), 0x60 DR.
//!
//! Timing: a frame goes out from the TX FIFO while the controller is enabled
//! and SER selects a slave, and takes DFS + 1 SCLK periods, SCLK being the
//! bus clock / SCKDV (22.10.6; SCKDV 0 stops it). Its received byte enters
//! the RX FIFO when it's done (not in TMOD 01, transmit only). A write to a
//! full TX FIFO is dropped (TXO), as is a frame into a full RX FIFO (RXO).
//! FIFO depth: SPIM2's is 4 (22.2, and its 2-bit thresholds); QSPI's says
//! 10 (23.2) but its TXFLR is 3 bits, so 8 until the board settles it
//! (BOARD_FACTS.md). The IRQ line is high while ISR != 0. Not modelled: DMA,
//! frame formats other than Motorola SPI.
//!
//! What's on the wire is the board's: an [`SsiDevice`].

use crate::chip::Write;
use crate::gpio::Gpio;
use cortex_m3_emu::TICK_HZ;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub const QSPI_FIFO_DEPTH: usize = 8;
pub const SPIM2_FIFO_DEPTH: usize = 4;
pub const QSPI_IRQ: u32 = 26;
pub const SPIM2_IRQ: u32 = 27;

const CR0: u32 = 0x00;
const CR1: u32 = 0x04;
const SPIENR: u32 = 0x08;
const SER: u32 = 0x10;
const BAUDR: u32 = 0x14;
const TXFTLR: u32 = 0x18;
const RXFTLR: u32 = 0x1C;
const TXFLR: u32 = 0x20;
const RXFLR: u32 = 0x24;
const SR: u32 = 0x28;
const IER: u32 = 0x2C;
const ISR: u32 = 0x30;
const RISR: u32 = 0x34;
const TXOICR: u32 = 0x38;
const RXOICR: u32 = 0x3C;
const RXUICR: u32 = 0x40;
const ICR: u32 = 0x48;
const DR: u32 = 0x60;

const CR0_RESET: u32 = 0x0100_0007;
const IER_RESET: u32 = 0x1F;
const TMOD_TX_ONLY: u32 = 1 << 8;

/// The controller's set-up and the wire's timing when a frame has gone out.
#[derive(Clone, Copy, Debug, Default)]
pub struct SsiLine {
    pub cr0: u32,
    pub baudr: u32,
    /// SCLK, in Hz.
    pub sclk_hz: u64,
    /// How long the frame took, in core ticks.
    pub frame: u64,
    /// When it finished.
    pub end: u64,
}

/// What hangs off the controller's pins.
pub trait SsiDevice {
    /// One frame shifted out on MOSI, done at `line.end`; returns the byte
    /// on MISO. `gpio` shows the chip's pins (a chip select on a GPIO).
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
    depth: usize,
    cr0: u32,
    cr1: u32,
    spienr: u32,
    ser: u32,
    baudr: u32,
    txftlr: u32,
    rxftlr: u32,
    ier: u32,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    /// The frame on the wire and when it's done.
    shifting: Option<(u8, u64)>,
    txo: bool,
    rxu: bool,
    rxo: bool,
    /// The bus clock, in Hz; 0 while it's off.
    hz: u64,
    /// Firmware misuse (the models' ErrorLog lines).
    pub errors: Vec<String>,
    pub device: Option<Box<dyn SsiDevice>>,
}

impl Ssi {
    pub fn new(block: &'static str, irq: u32, depth: usize) -> Self {
        Ssi {
            block,
            irq,
            depth,
            cr0: CR0_RESET,
            cr1: 0,
            spienr: 0,
            ser: 0,
            baudr: 0,
            txftlr: 0,
            rxftlr: 0,
            ier: IER_RESET,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            shifting: None,
            txo: false,
            rxu: false,
            rxo: false,
            hz: 0,
            errors: Vec::new(),
            device: None,
        }
    }

    /// A system reset: the registers go back; the device and the clock
    /// (the chip's to set) stay.
    pub fn reset(&mut self) {
        let device = self.device.take();
        *self = Ssi {
            device,
            hz: self.hz,
            ..Ssi::new(self.block, self.irq, self.depth)
        };
    }

    /// The bus clock changed (0: gated). A frame on the wire keeps its end.
    pub fn set_clock(&mut self, hz: u64, now: u64) {
        self.hz = hz;
        self.start(now);
    }

    pub fn hz(&self) -> u64 {
        self.hz
    }

    fn bits(&self) -> u64 {
        (self.cr0 & 0xF) as u64 + 1
    }

    fn sclk_hz(&self) -> u64 {
        let div = (self.baudr & 0xFFFF) as u64;
        if div == 0 { 0 } else { self.hz / div }
    }

    /// Core ticks a frame takes; `None` while SCLK doesn't run.
    fn frame_ticks(&self) -> Option<u64> {
        let div = (self.baudr & 0xFFFF) as u64;
        (self.hz != 0 && div != 0)
            .then(|| (TICK_HZ as u128 * (div * self.bits()) as u128 / self.hz as u128) as u64)
    }

    /// Starts the next frame at `now` if one can go.
    fn start(&mut self, now: u64) {
        if self.shifting.is_some() || self.spienr & 1 == 0 || self.ser == 0 {
            return;
        }
        let Some(t) = self.frame_ticks() else { return };
        if let Some(b) = self.tx.pop_front() {
            self.shifting = Some((b, now + t));
        }
    }

    /// When the frame on the wire is done.
    pub fn next_event(&self) -> u64 {
        self.shifting.map_or(u64::MAX, |(_, end)| end)
    }

    /// Finishes the frames due by `now`, each starting the next.
    pub fn advance(&mut self, now: u64, gpio: &Gpio) {
        while let Some((b, end)) = self.shifting.filter(|&(_, end)| end <= now) {
            self.shifting = None;
            let line = SsiLine {
                cr0: self.cr0,
                baudr: self.baudr,
                sclk_hz: self.sclk_hz(),
                frame: self.frame_ticks().unwrap_or(0),
                end,
            };
            let r = match &mut self.device {
                Some(d) => d.shift(b, line, gpio),
                None => 0xFF,
            };
            if self.cr0 & 0x300 != TMOD_TX_ONLY {
                if self.rx.len() < self.depth {
                    self.rx.push_back(r);
                } else {
                    self.rxo = true;
                }
            }
            self.start(end);
        }
    }

    fn risr(&self) -> u32 {
        (self.tx.len() <= self.txftlr as usize) as u32
            | (self.txo as u32) << 1
            | (self.rxu as u32) << 2
            | (self.rxo as u32) << 3
            | ((self.rx.len() > self.rxftlr as usize) as u32) << 4
    }

    /// Bit `irq` while ISR != 0.
    pub fn irq_level(&self) -> u64 {
        ((self.risr() & self.ier != 0) as u64) << self.irq
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            DR => match self.rx.pop_front() {
                Some(b) => b as u32,
                None => {
                    self.rxu = true;
                    0
                }
            },
            TXFLR => self.tx.len() as u32,
            RXFLR => self.rx.len() as u32,
            SR => {
                let busy = self.shifting.is_some() || !self.tx.is_empty();
                busy as u32
                    | ((self.tx.len() < self.depth) as u32) << 1
                    | (self.tx.is_empty() as u32) << 2
                    | (!self.rx.is_empty() as u32) << 3
                    | ((self.rx.len() >= self.depth) as u32) << 4
            }
            ISR => self.risr() & self.ier,
            RISR => self.risr(),
            TXOICR => std::mem::take(&mut self.txo) as u32,
            RXOICR => std::mem::take(&mut self.rxo) as u32,
            RXUICR => std::mem::take(&mut self.rxu) as u32,
            ICR => {
                let any = self.txo | self.rxo | self.rxu;
                (self.txo, self.rxo, self.rxu) = (false, false, false);
                any as u32
            }
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

    pub fn write(&mut self, off: u32, v: u32, now: u64, gpio: &Gpio, log: &mut impl FnMut(Write)) {
        if off == DR {
            if self.spienr & 1 == 0 {
                self.errors.push(format!(
                    "{}: DR write while SPIENR = 0, ignored",
                    self.block.to_lowercase()
                ));
                return;
            }
            self.advance(now, gpio);
            if self.tx.len() < self.depth {
                self.tx.push_back(v as u8);
            } else {
                self.txo = true;
            }
            self.start(now);
            return;
        }
        log(Write {
            block: self.block,
            off,
            value: v,
        });
        match off {
            SPIENR => {
                self.spienr = v & 1;
                if v & 1 == 0 {
                    self.tx.clear();
                    self.rx.clear();
                    self.shifting = None;
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
            TXFTLR if (v as usize) < self.depth => self.txftlr = v,
            RXFTLR if (v as usize) < self.depth => self.rxftlr = v,
            SER => self.ser = v,
            IER => self.ier = v & 0x1F,
            _ => {}
        }
        self.start(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo(Rc<RefCell<Vec<(u8, u64)>>>);
    impl SsiDevice for Echo {
        fn shift(&mut self, b: u8, line: SsiLine, _: &Gpio) -> u8 {
            self.0.borrow_mut().push((b, line.end));
            !b
        }
    }

    const US: u64 = TICK_HZ / 1_000_000;

    /// SPIM2 at 96 MHz / 32 = 3 MHz, 8-bit frames: 8/3 us each.
    fn spim2() -> (Ssi, Rc<RefCell<Vec<(u8, u64)>>>, Gpio) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut s = Ssi::new("SPIM2", SPIM2_IRQ, SPIM2_FIFO_DEPTH);
        s.device = Some(Box::new(Echo(seen.clone())));
        s.set_clock(96_000_000, 0);
        let gpio = Gpio::default();
        let mut log = |_| {};
        s.write(BAUDR, 32, 0, &gpio, &mut log);
        s.write(IER, 0, 0, &gpio, &mut log);
        s.write(SER, 1, 0, &gpio, &mut log);
        s.write(SPIENR, 1, 0, &gpio, &mut log);
        (s, seen, gpio)
    }

    #[test]
    fn reset_values_and_the_lock_while_enabled() {
        let gpio = Gpio::default();
        let mut s = Ssi::new("QSPI", QSPI_IRQ, QSPI_FIFO_DEPTH);
        assert_eq!(
            (s.read(CR0), s.read(IER), s.read(SR)),
            (CR0_RESET, 0x1F, 0x06)
        );
        let mut log = |_| {};
        s.write(DR, 1, 0, &gpio, &mut log);
        assert_eq!(s.errors.len(), 1);
        s.write(BAUDR, 32, 0, &gpio, &mut log);
        s.write(SPIENR, 1, 0, &gpio, &mut log);
        s.write(BAUDR, 32, 0, &gpio, &mut log);
        assert_eq!(s.errors.len(), 1, "an unchanged value is fine");
        s.write(BAUDR, 16, 0, &gpio, &mut log);
        assert_eq!((s.errors.len(), s.read(BAUDR)), (2, 32));
        // The depth probe: thresholds of the depth and up are dropped.
        s.write(RXFTLR, 8, 0, &gpio, &mut log);
        s.write(RXFTLR, 7, 0, &gpio, &mut log);
        assert_eq!(s.read(RXFTLR), 7);
        let mut m = Ssi::new("SPIM2", SPIM2_IRQ, SPIM2_FIFO_DEPTH);
        m.write(TXFTLR, 4, 0, &gpio, &mut log);
        m.write(TXFTLR, 3, 0, &gpio, &mut log);
        assert_eq!(m.read(TXFTLR), 3);
    }

    #[test]
    fn frames_take_their_sclk_periods_and_the_fifo_holds_four() {
        let (mut s, seen, gpio) = spim2();
        let frame = 8 * 32 * TICK_HZ / 96_000_000;
        let mut log = |_| {};
        // One on the wire at once, four in the FIFO, the sixth dropped.
        for b in 1..=6 {
            s.write(DR, b, 0, &gpio, &mut log);
        }
        assert_eq!((s.read(TXFLR), s.read(SR) & 0x7), (4, 0x1), "busy, full");
        assert_eq!(s.read(RISR) & 0x3, 0x2, "TXO, TXE not yet");
        assert_eq!(s.read(TXOICR), 1);
        assert_eq!(s.next_event(), frame);
        s.advance(frame - 1, &gpio);
        assert!(seen.borrow().is_empty());
        s.advance(5 * frame, &gpio);
        let ends: Vec<u64> = (1..=5).map(|n| n * frame).collect();
        assert_eq!(*seen.borrow(), (1..=5).zip(ends).collect::<Vec<_>>());
        assert_eq!(s.read(SR) & 0x7, 0x6, "idle, empty");
        // RX: four fit, the fifth overflowed.
        assert_eq!((s.read(RXFLR), s.read(RISR) & 0x18), (4, 0x18));
        assert_eq!(s.read(DR), 0xFE);
        assert_eq!(s.read(RXOICR), 1);
        // 8/3 us a frame at 3 MHz.
        assert_eq!(frame, 8 * US / 3);
    }

    #[test]
    fn nothing_moves_without_ser_sclk_or_a_clock_and_disabling_flushes() {
        let (mut s, seen, gpio) = spim2();
        let mut log = |_| {};
        s.write(SER, 0, 0, &gpio, &mut log);
        s.write(DR, 1, 0, &gpio, &mut log);
        assert_eq!(s.next_event(), u64::MAX, "no slave selected");
        s.write(SER, 1, 10, &gpio, &mut log);
        assert_ne!(s.next_event(), u64::MAX);
        s.set_clock(0, 10);
        s.advance(u64::MAX - 1, &gpio);
        s.write(DR, 2, 20, &gpio, &mut log);
        assert_eq!(s.next_event(), u64::MAX, "its clock is off");
        s.write(SPIENR, 0, 30, &gpio, &mut log);
        assert_eq!((s.read(TXFLR), s.read(RXFLR)), (0, 0));
        assert_eq!(seen.borrow().len(), 1);
        // Transmit only: nothing comes back.
        s.set_clock(96_000_000, 40);
        s.write(CR0, 0x107, 40, &gpio, &mut log);
        s.write(SPIENR, 1, 40, &gpio, &mut log);
        s.write(DR, 3, 40, &gpio, &mut log);
        s.advance(1 << 40, &gpio);
        assert_eq!((seen.borrow().len(), s.read(RXFLR)), (2, 0));
        assert_eq!(s.read(DR), 0);
        assert_eq!(s.read(RXUICR), 1, "underflow");
    }
}
