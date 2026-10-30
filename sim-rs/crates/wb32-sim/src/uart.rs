//! UART3 at 0x4000_8400 (IRQ 32, on APB2): the DesignWare 16550 of
//! RM2905025 chapter 25 (wb32fq95xx.h UART_TypeDef):
//!   0x00 RBR/THR (DLL while LCR.DLAB), 0x04 IER (& 0x9F; DLH while DLAB),
//!   0x08 IIR on read, FCR on write, 0x0C LCR, 0x10 MCR, 0x14 LSR (DR, OE,
//!   THRE, TEMT), 0x1C SCR, 0x7C USR (BUSY, TFNF, TFE, RFNE, RFF), 0x80 TFL,
//!   0x84 RFL, 0x88 SRR, 0x98 SFE, 0x9C SRT, 0xA0 STET, 0xC0 DLF.
//!
//! Timing: a character is its start, data (LCR WLS), parity and stop bits,
//! each 16 periods of PCLK2 / (DLH:DLL + DLF/16) (25.1.3); at 96 MHz with
//! DLL 52, DLF 1 that's 115,246 baud, 86.8 us for 8N1. With the FIFOs on
//! (FCR[0], or its shadow SFE) TX and RX each hold 16 (the RM's trigger
//! levels, 1/4/8/14 and 0/2/4/8, are the DW 16550's for 16 deep; its 4-bit
//! TFL/RFL can't count 16, the same slip as QSPI's), otherwise 1. A byte
//! written to a full THR/FIFO is lost and reported. The peer gets each byte
//! when its stop bit ends; the bytes the peer puts on `wire` arrive one
//! character time each, from when the line is free, into the RX FIFO. One
//! arriving at a full FIFO is lost and sets LSR.OE; one arriving while
//! UART3 has no clock (stop mode) is lost unseen. The line's level, start
//! bit first, is `rx_level`: the chip puts it on C11, so EXTI 11 sees it.
//!
//! Interrupts (IIR, table 25-2, highest first): receive line status (OE,
//! with RLSIE), received data at the trigger level (RDAIE), character
//! timeout (data below the trigger and 4 characters of quiet), THR empty
//! (THREIE: raised when the TX FIFO empties, or THREIE is set while it's
//! empty; cleared by an IIR read that reports it or a THR write). The IRQ
//! line is high while IIR has one. Not modelled: PTIME, parity and framing
//! errors, break, the modem lines, 9-bit mode, DMA.
//!
//! What's on the other end is the board's: a [`UartPeer`].

use crate::chip::Write;
use crate::gpio::{Gpio, Pin};
use cortex_m3_emu::TICK_HZ;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub const UART3_IRQ: u32 = 32;
pub const FIFO_DEPTH: usize = 16;
const IER_RDAIE: u32 = 0x1;
const IER_THREIE: u32 = 0x2;
const IER_RLSIE: u32 = 0x4;
const IER_LSRCLRMD: u32 = 0x10;
const LCR_DLAB: u32 = 0x80;
const RX_TRIGGER: [usize; 4] = [1, 4, 8, 14];
/// 8N1 at 115,200 baud: how long a byte takes on the wire while the chip
/// can't say (no clock, no divisor).
const DEFAULT_CHAR: u64 = 10 * TICK_HZ / 115_200;

/// The line set-up when a byte goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartLine {
    pub dll: u32,
    pub dlh: u32,
    pub dlf: u32,
    pub lcr: u32,
    pub sfe: u32,
}

/// The other end of the line. Bytes it sends go onto `wire`.
pub trait UartPeer {
    /// A byte the chip sent, its stop bit done at `now`. `gpio` shows the
    /// chip's pins (their alternate functions).
    fn tx(&mut self, b: u8, line: UartLine, now: u64, gpio: &Gpio, wire: &mut VecDeque<u8>);

    /// When it next has something to do by itself.
    fn next_event(&self) -> u64 {
        u64::MAX
    }

    /// Does what's due at `now`.
    fn advance(&mut self, _now: u64, _wire: &mut VecDeque<u8>) {}
}

impl<T: UartPeer> UartPeer for Rc<RefCell<T>> {
    fn tx(&mut self, b: u8, line: UartLine, now: u64, gpio: &Gpio, wire: &mut VecDeque<u8>) {
        self.borrow_mut().tx(b, line, now, gpio, wire)
    }
    fn next_event(&self) -> u64 {
        self.borrow().next_event()
    }
    fn advance(&mut self, now: u64, wire: &mut VecDeque<u8>) {
        self.borrow_mut().advance(now, wire)
    }
}

pub struct Uart {
    pub block: &'static str,
    irq: u32,
    ier: u32,
    lcr: u32,
    mcr: u32,
    scr: u32,
    fifo: bool,
    rx_trig: u32,
    tx_trig: u32,
    dlf: u32,
    dll: u32,
    dlh: u32,
    tx: VecDeque<u8>,
    /// The byte going out and when its stop bit ends.
    tx_shift: Option<(u8, u64)>,
    rx: VecDeque<u8>,
    /// The byte coming in and when its start bit began.
    rx_shift: Option<(u8, u64)>,
    oe: bool,
    thre_irq: bool,
    /// When the receiver last saw a byte or RBR was read (the character
    /// timeout's count).
    rx_quiet_since: u64,
    /// The bus clock, in Hz; 0 while it's off.
    hz: u64,
    now: u64,
    /// Bytes the far end has put on the line, not yet arriving.
    pub wire: VecDeque<u8>,
    /// Bytes that arrived while UART3 had no clock.
    pub lost: u32,
    /// Firmware misuse.
    pub errors: Vec<String>,
    pub peer: Option<Box<dyn UartPeer>>,
    /// The pin the far end's TX is wired to (the board's), which the chip
    /// drives with `rx_level`.
    pub rx_pin: Option<Pin>,
}

impl Uart {
    pub fn new(block: &'static str, irq: u32) -> Self {
        Uart {
            block,
            irq,
            ier: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            fifo: false,
            rx_trig: 0,
            tx_trig: 0,
            dlf: 0,
            dll: 0,
            dlh: 0,
            tx: VecDeque::new(),
            tx_shift: None,
            rx: VecDeque::new(),
            rx_shift: None,
            oe: false,
            thre_irq: false,
            rx_quiet_since: 0,
            hz: 0,
            now: 0,
            wire: VecDeque::new(),
            lost: 0,
            errors: Vec::new(),
            peer: None,
            rx_pin: None,
        }
    }

    /// A system reset: the registers and FIFOs go; the peer, what it has
    /// on the line, the clock (the chip's to set) and the counts stay.
    pub fn reset(&mut self) {
        let fresh = Uart {
            peer: self.peer.take(),
            rx_pin: self.rx_pin,
            wire: std::mem::take(&mut self.wire),
            rx_shift: self.rx_shift,
            hz: self.hz,
            now: self.now,
            lost: self.lost,
            errors: std::mem::take(&mut self.errors),
            ..Uart::new(self.block, self.irq)
        };
        *self = fresh;
    }

    pub fn set_clock(&mut self, hz: u64, now: u64, gpio: &Gpio) {
        self.advance_to(now, gpio);
        self.hz = hz;
        self.start_tx(now);
    }

    pub fn hz(&self) -> u64 {
        self.hz
    }

    fn depth(&self) -> usize {
        if self.fifo { FIFO_DEPTH } else { 1 }
    }

    fn line(&self) -> UartLine {
        UartLine {
            dll: self.dll,
            dlh: self.dlh,
            dlf: self.dlf,
            lcr: self.lcr,
            sfe: self.fifo as u32,
        }
    }

    /// Bits a character takes: start, data, parity, stop.
    fn bits(&self) -> u64 {
        let data = 5 + (self.lcr & 3) as u64;
        let parity = (self.lcr >> 3 & 1) as u64;
        let stop = 1 + (self.lcr >> 2 & 1) as u64;
        1 + data + parity + stop
    }

    /// Core ticks per bit; `None` while the baud clock doesn't run.
    fn bit_ticks(&self) -> Option<u64> {
        let div16 = 16 * (self.dlh << 8 | self.dll) as u64 + (self.dlf & 0xF) as u64;
        (self.hz != 0 && self.dll | self.dlh != 0)
            .then(|| (TICK_HZ as u128 * div16 as u128 / self.hz as u128) as u64)
    }

    /// How long a character takes on the wire. The far end keeps its own
    /// rate, so while the chip's baud clock is off, 8N1 at 115,200.
    fn char_ticks(&self) -> u64 {
        self.bit_ticks().map_or(DEFAULT_CHAR, |b| b * self.bits())
    }

    fn rx_trigger(&self) -> usize {
        if self.fifo {
            RX_TRIGGER[(self.rx_trig & 3) as usize]
        } else {
            1
        }
    }

    fn cto_at(&self) -> Option<u64> {
        (self.fifo && !self.rx.is_empty() && self.rx.len() < self.rx_trigger())
            .then(|| self.rx_quiet_since + 4 * self.char_ticks())
    }

    /// The pending interrupt, as IIR[3:0].
    fn intid(&self) -> u32 {
        if self.ier & IER_RLSIE != 0 && self.oe {
            0x6
        } else if self.ier & IER_RDAIE != 0 && self.rx.len() >= self.rx_trigger() {
            0x4
        } else if self.ier & IER_RDAIE != 0 && self.cto_at().is_some_and(|t| t <= self.now) {
            0xC
        } else if self.ier & IER_THREIE != 0 && self.thre_irq && self.tx.is_empty() {
            0x2
        } else {
            0x1
        }
    }

    pub fn irq_level(&self) -> u64 {
        ((self.intid() != 1) as u64) << self.irq
    }

    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    /// The RX line now: idle high, or the bits of the byte arriving.
    pub fn rx_level(&self) -> bool {
        let Some((b, start)) = self.rx_shift else {
            return true;
        };
        let bit = self.bit_ticks().unwrap_or(DEFAULT_CHAR / 10);
        let i = (self.now.saturating_sub(start) / bit) as u32;
        let data = 5 + (self.lcr & 3);
        match i {
            0 => false,
            i if i <= data => b >> (i - 1) & 1 != 0,
            _ => true,
        }
    }

    /// When the RX line's level may next change.
    fn rx_edge(&self) -> u64 {
        let Some((_, start)) = self.rx_shift else {
            return u64::MAX;
        };
        let bit = self.bit_ticks().unwrap_or(DEFAULT_CHAR / 10);
        start + (self.now.saturating_sub(start) / bit + 1) * bit
    }

    fn start_tx(&mut self, now: u64) {
        if self.tx_shift.is_some() {
            return;
        }
        let Some(bit) = self.bit_ticks() else { return };
        if let Some(b) = self.tx.pop_front() {
            self.tx_shift = Some((b, now + bit * self.bits()));
            if self.tx.is_empty() {
                self.thre_irq = true;
            }
        }
    }

    fn start_rx(&mut self, now: u64) {
        if self.rx_shift.is_none()
            && let Some(b) = self.wire.pop_front()
        {
            self.rx_shift = Some((b, now));
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 if self.dlab() => self.dll,
            0x00 => {
                self.rx_quiet_since = self.now;
                if self.ier & IER_LSRCLRMD == 0 {
                    self.oe = false;
                }
                self.rx.pop_front().unwrap_or(0) as u32
            }
            0x04 if self.dlab() => self.dlh,
            0x04 => self.ier,
            0x08 => {
                let id = self.intid();
                if id == 0x2 {
                    self.thre_irq = false;
                }
                id | if self.fifo { 0xC0 } else { 0 }
            }
            0x0C => self.lcr,
            0x10 => self.mcr,
            0x14 => {
                let temt = self.tx.is_empty() && self.tx_shift.is_none();
                let v = !self.rx.is_empty() as u32
                    | (std::mem::take(&mut self.oe) as u32) << 1
                    | (self.tx.is_empty() as u32) << 5
                    | (temt as u32) << 6;
                v
            }
            0x1C => self.scr,
            0x7C => {
                let busy =
                    self.tx_shift.is_some() || !self.tx.is_empty() || self.rx_shift.is_some();
                busy as u32
                    | ((self.tx.len() < self.depth()) as u32) << 1
                    | (self.tx.is_empty() as u32) << 2
                    | (!self.rx.is_empty() as u32) << 3
                    | ((self.rx.len() >= self.depth()) as u32) << 4
            }
            0x80 => self.tx.len() as u32,
            0x84 => self.rx.len() as u32,
            0x98 => self.fifo as u32,
            0x9C => self.rx_trig,
            0xA0 => self.tx_trig,
            0xC0 => self.dlf,
            _ => 0,
        }
    }

    fn set_fifo(&mut self, on: bool) {
        if on != self.fifo {
            self.fifo = on;
            self.tx.clear();
            self.rx.clear();
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64, gpio: &Gpio, log: &mut impl FnMut(Write)) {
        self.advance_to(now, gpio);
        if off == 0x00 && !self.dlab() {
            if self.tx.len() < self.depth() {
                self.tx.push_back(v as u8);
            } else {
                self.errors.push(format!(
                    "{}: THR write 0x{:02x} with the TX FIFO full, lost",
                    self.block.to_lowercase(),
                    v & 0xFF
                ));
            }
            self.thre_irq = false;
            self.start_tx(now);
            return;
        }
        log(Write {
            block: self.block,
            off,
            value: v,
        });
        match off {
            0x00 => self.dll = v & 0xFF,
            0x04 if self.dlab() => self.dlh = v & 0xFF,
            0x04 => {
                if v & IER_THREIE != 0 && self.ier & IER_THREIE == 0 && self.tx.is_empty() {
                    self.thre_irq = true;
                }
                self.ier = v & 0x9F;
            }
            0x08 => {
                self.set_fifo(v & 1 != 0);
                if v & 2 != 0 {
                    self.rx.clear();
                }
                if v & 4 != 0 {
                    self.tx.clear();
                }
                self.rx_trig = v >> 6 & 3;
                self.tx_trig = v >> 4 & 3;
            }
            0x0C => self.lcr = v & 0xFF,
            0x10 => self.mcr = v & 0x62,
            0x1C => self.scr = v & 0xFF,
            0x88 => {
                if v & 1 != 0 {
                    self.reset();
                    return;
                }
                if v & 2 != 0 {
                    self.rx.clear();
                }
                if v & 4 != 0 {
                    self.tx.clear();
                }
            }
            0x98 => self.set_fifo(v & 1 != 0),
            0x9C => self.rx_trig = v & 3,
            0xA0 => self.tx_trig = v & 3,
            0xC0 => self.dlf = v & 0xF,
            _ => {}
        }
        self.start_tx(now);
    }

    pub fn next_event(&self) -> u64 {
        let peer = self.peer.as_ref().map_or(u64::MAX, |p| p.next_event());
        let rx_end = match self.rx_shift {
            Some((_, start)) => start + self.char_ticks(),
            // Bytes on the wire start arriving at once.
            None if !self.wire.is_empty() => self.now,
            None => u64::MAX,
        };
        let edge = if self.rx_pin.is_some() {
            self.rx_edge()
        } else {
            u64::MAX
        };
        [
            peer,
            rx_end,
            edge,
            self.tx_shift.map_or(u64::MAX, |(_, end)| end),
            self.cto_at().unwrap_or(u64::MAX),
        ]
        .into_iter()
        .min()
        .unwrap()
    }

    /// Does what's due by `now`, in time order.
    pub fn advance(&mut self, now: u64, gpio: &Gpio) {
        self.advance_to(now, gpio);
    }

    fn advance_to(&mut self, now: u64, gpio: &Gpio) {
        // What the far end put on the line since the last call starts then.
        self.start_rx(self.now);
        loop {
            let tx_end = self.tx_shift.map_or(u64::MAX, |(_, end)| end);
            let rx_end = self
                .rx_shift
                .map_or(u64::MAX, |(_, s)| s + self.char_ticks());
            let peer = self.peer.as_ref().map_or(u64::MAX, |p| p.next_event());
            let t = tx_end.min(rx_end).min(peer);
            if t > now {
                break;
            }
            self.now = self.now.max(t);
            if t == rx_end {
                let (b, _) = self.rx_shift.take().unwrap();
                if self.hz == 0 {
                    self.lost += 1;
                } else if self.rx.len() < self.depth() {
                    self.rx.push_back(b);
                } else {
                    self.oe = true;
                }
                self.rx_quiet_since = t;
            } else if t == tx_end {
                let (b, _) = self.tx_shift.take().unwrap();
                let line = self.line();
                if let Some(p) = &mut self.peer {
                    p.tx(b, line, t, gpio, &mut self.wire);
                }
                self.start_tx(t);
            } else if let Some(p) = &mut self.peer {
                p.advance(t, &mut self.wire);
            }
            self.start_rx(t);
        }
        self.now = self.now.max(now);
        self.start_rx(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: u64 = TICK_HZ / 1_000_000;

    /// Echoes each byte back, plus one.
    struct Echo(Rc<RefCell<Vec<(u8, u64)>>>);
    impl UartPeer for Echo {
        fn tx(&mut self, b: u8, _: UartLine, now: u64, _: &Gpio, wire: &mut VecDeque<u8>) {
            self.0.borrow_mut().push((b, now));
            wire.push_back(b.wrapping_add(1));
        }
    }

    /// QMK's and our set-up: 96 MHz, DLL 52, DLF 1 (115,246 baud), 8N1,
    /// FIFOs on through SFE, RX trigger 1.
    fn uart3() -> (Uart, Rc<RefCell<Vec<(u8, u64)>>>, Gpio) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut u = Uart::new("UART3", UART3_IRQ);
        u.peer = Some(Box::new(Echo(seen.clone())));
        let gpio = Gpio::default();
        u.set_clock(96_000_000, 0, &gpio);
        let mut log = |_| {};
        for (o, v) in [
            (0xC0, 1),
            (0x0C, 0x80),
            (0x00, 52),
            (0x04, 0),
            (0x0C, 3),
            (0x98, 1),
        ] {
            u.write(o, v, 0, &gpio, &mut log);
        }
        (u, seen, gpio)
    }

    #[test]
    fn a_byte_takes_its_ten_bits_at_the_fractional_baud() {
        let (mut u, seen, gpio) = uart3();
        // 16 x 52 1/16 / 96 MHz a bit: 8.68 us, 86.8 us a character.
        let char = 10 * (TICK_HZ as u128 * 833 / 96_000_000) as u64;
        assert_eq!(char / US, 86);
        let mut log = |_| {};
        u.write(0x00, 0x41, 0, &gpio, &mut log);
        u.write(0x00, 0x42, 0, &gpio, &mut log);
        assert_eq!((u.read(0x80), u.read(0x14) & 0x60), (1, 0x00));
        assert_eq!(u.read(0x7C) & 1, 1, "busy");
        u.advance(char - 1, &gpio);
        assert!(seen.borrow().is_empty());
        u.advance(char, &gpio);
        assert_eq!(*seen.borrow(), [(0x41, char)]);
        assert_eq!(u.read(0x14) & 0x60, 0x20, "THRE, not TEMT");
        u.advance(2 * char, &gpio);
        assert_eq!(seen.borrow()[1], (0x42, 2 * char));
        // The echoes follow at the line's rate: 0x42 at 2 chars, 0x43 at 3.
        assert_eq!(u.read(0x84), 1);
        u.advance(3 * char, &gpio);
        assert_eq!((u.read(0x84), u.read(0x14) & 0x61), (2, 0x61));
        assert_eq!((u.read(0x00), u.read(0x00)), (0x42, 0x43));
    }

    #[test]
    fn the_rx_line_shows_start_data_and_stop_bits() {
        let (mut u, _, gpio) = uart3();
        u.wire.push_back(0b0000_0101);
        u.advance(0, &gpio);
        let bit = (TICK_HZ as u128 * 833 / 96_000_000) as u64;
        let levels: Vec<bool> = (0..10)
            .map(|i| {
                u.advance(i * bit + bit / 2, &gpio);
                u.rx_level()
            })
            .collect();
        // Start, then LSB first, then stop.
        let want = [
            false, true, false, true, false, false, false, false, false, true,
        ];
        assert_eq!(levels, want);
        assert_eq!(u.next_event(), 10 * bit, "the end of the stop bit");
    }

    #[test]
    fn overrun_sets_oe_and_a_byte_without_a_clock_is_lost() {
        let (mut u, _, gpio) = uart3();
        u.write(0x04, IER_RLSIE | IER_RDAIE, 0, &gpio, &mut |_| {});
        u.wire.extend(0..17);
        u.advance(1 << 40, &gpio);
        assert_eq!(u.read(0x84), 16);
        assert_eq!(u.read(0x08) & 0xF, 0x6, "line status first");
        assert_eq!(u.read(0x14) & 0x3, 0x3, "DR and OE, cleared by the read");
        assert_eq!(u.read(0x08) & 0xF, 0x4, "then data");
        // Stopped: the clock off, a byte goes by unseen.
        u.set_clock(0, 1 << 40, &gpio);
        u.wire.push_back(0xAA);
        u.advance(1 << 41, &gpio);
        assert_eq!((u.read(0x84), u.lost), (16, 1));
    }

    #[test]
    fn thre_raises_on_empty_and_clears_on_iir_read_or_a_write() {
        let (mut u, _, gpio) = uart3();
        let mut log = |_| {};
        u.write(0x04, IER_THREIE, 0, &gpio, &mut log);
        assert_eq!(u.irq_level(), 1 << UART3_IRQ, "set while THR is empty");
        assert_eq!(u.read(0x08), 0xC2);
        assert_eq!(
            (u.irq_level(), u.read(0x08)),
            (0, 0xC1),
            "the IIR read cleared it"
        );
        u.write(0x00, 1, 0, &gpio, &mut log);
        u.write(0x00, 2, 0, &gpio, &mut log);
        assert_eq!(u.irq_level(), 0);
        // The FIFO empties as the second byte starts out.
        u.advance(1 << 30, &gpio);
        assert_eq!(u.read(0x08) & 0xF, 0x2);
        // Without the FIFOs THR holds one: a second write is lost.
        u.write(0x98, 0, 1 << 31, &gpio, &mut log);
        u.write(0x00, 3, 1 << 31, &gpio, &mut log);
        u.write(0x00, 4, 1 << 31, &gpio, &mut log);
        u.write(0x00, 5, 1 << 31, &gpio, &mut log);
        assert_eq!(
            u.errors,
            ["uart3: THR write 0x05 with the TX FIFO full, lost"]
        );
    }
}
