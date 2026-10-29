//! UART3 at 0x4000_8400 (IRQ 32): the DesignWare 16550 of wb32fq95xx.h's
//! UART_TypeDef. A port of the controller half of
//! ../../sim/peripherals/uart3.py:
//!   0x00 RBR/THR (DLL while LCR.DLAB), 0x04 IER (& 0x8F; DLH while DLAB),
//!   0x08 IIR (4 data waiting, else 2 with THREIE, else 1), 0x0C LCR,
//!   0x10 MCR, 0x14 LSR (THRE, TEMT; DR while a byte waits), 0x7C USR
//!   (TFNF, TFE; RFNE while a byte waits), 0x80 TFL 0, 0x84 RFL, 0x98 SFE,
//!   0x9C SRT, 0xC0 DLF.
//! A byte goes out the moment THR is written, so THR is always empty;
//! received bytes queue without limit. The IRQ line is high while IER.RDAIE
//! and a byte waits, or while IER.THREIE. Not modelled: timing, line
//! errors, FIFO depths, the modem lines.
//!
//! What's on the other end is the board's: a [`UartPeer`].

use crate::chip::Write;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub const UART3_IRQ: u32 = 32;
const IER_RDAIE: u32 = 0x1;
const IER_THREIE: u32 = 0x2;
const LCR_DLAB: u32 = 0x80;

/// The line set-up when a byte goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartLine {
    pub dll: u32,
    pub dlh: u32,
    pub dlf: u32,
    pub lcr: u32,
    pub sfe: u32,
}

/// The other end of the line. Bytes it sends go into `rx`.
pub trait UartPeer {
    /// A byte the chip sent.
    fn tx(&mut self, b: u8, line: UartLine, now: u64, rx: &mut VecDeque<u8>);

    /// When it next has something to do by itself.
    fn next_event(&self) -> u64 {
        u64::MAX
    }

    /// Does what's due at `now`.
    fn advance(&mut self, _now: u64, _rx: &mut VecDeque<u8>) {}
}

impl<T: UartPeer> UartPeer for Rc<RefCell<T>> {
    fn tx(&mut self, b: u8, line: UartLine, now: u64, rx: &mut VecDeque<u8>) {
        self.borrow_mut().tx(b, line, now, rx)
    }
    fn next_event(&self) -> u64 {
        self.borrow().next_event()
    }
    fn advance(&mut self, now: u64, rx: &mut VecDeque<u8>) {
        self.borrow_mut().advance(now, rx)
    }
}

pub struct Uart {
    pub block: &'static str,
    irq: u32,
    ier: u32,
    lcr: u32,
    mcr: u32,
    sfe: u32,
    srt: u32,
    dlf: u32,
    dll: u32,
    dlh: u32,
    /// Received bytes waiting in RBR.
    pub rx: VecDeque<u8>,
    pub peer: Option<Box<dyn UartPeer>>,
}

impl Uart {
    pub fn new(block: &'static str, irq: u32) -> Self {
        Uart {
            block,
            irq,
            ier: 0,
            lcr: 0,
            mcr: 0,
            sfe: 0,
            srt: 0,
            dlf: 0,
            dll: 0,
            dlh: 0,
            rx: VecDeque::new(),
            peer: None,
        }
    }

    /// A system reset: registers and the RX queue go; the peer stays.
    pub fn reset(&mut self) {
        let peer = self.peer.take();
        *self = Uart {
            peer,
            ..Uart::new(self.block, self.irq)
        };
    }

    pub fn irq_level(&self) -> u64 {
        let level =
            (self.ier & IER_RDAIE != 0 && !self.rx.is_empty()) || self.ier & IER_THREIE != 0;
        (level as u64) << self.irq
    }

    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        let waiting = !self.rx.is_empty() as u32;
        match off {
            0x00 if self.dlab() => self.dll,
            0x00 => self.rx.pop_front().unwrap_or(0) as u32,
            0x04 if self.dlab() => self.dlh,
            0x04 => self.ier,
            0x08 if waiting != 0 => 0x04,
            0x08 if self.ier & IER_THREIE != 0 => 0x02,
            0x08 => 0x01,
            0x0C => self.lcr,
            0x10 => self.mcr,
            0x14 => 0x60 | waiting,
            0x7C => 0x06 | waiting << 3,
            0x84 => self.rx.len() as u32,
            0x98 => self.sfe,
            0x9C => self.srt,
            0xC0 => self.dlf,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64, log: &mut impl FnMut(Write)) {
        if off == 0x00 {
            if self.dlab() {
                self.dll = v & 0xFF;
            } else {
                let line = UartLine {
                    dll: self.dll,
                    dlh: self.dlh,
                    dlf: self.dlf,
                    lcr: self.lcr,
                    sfe: self.sfe,
                };
                if let Some(p) = &mut self.peer {
                    p.tx(v as u8, line, now, &mut self.rx);
                }
            }
            return;
        }
        log(Write {
            block: self.block,
            off,
            value: v,
        });
        match off {
            0x04 if self.dlab() => self.dlh = v & 0xFF,
            0x04 => self.ier = v & 0x8F,
            0x0C => self.lcr = v,
            0x10 => self.mcr = v,
            0x98 => self.sfe = v,
            0x9C => self.srt = v,
            0xC0 => self.dlf = v,
            _ => {}
        }
    }

    pub fn next_event(&self) -> u64 {
        self.peer.as_ref().map_or(u64::MAX, |p| p.next_event())
    }

    pub fn advance(&mut self, now: u64) {
        if let Some(p) = &mut self.peer
            && p.next_event() <= now
        {
            p.advance(now, &mut self.rx);
        }
    }
}
