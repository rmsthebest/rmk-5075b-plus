//! UART3 on C10 (TX) / C11 (RX), 8N1, interrupt driven: the radio module's
//! link (QMK's `uart_serial.c` on ChibiOS-contrib's WB32 serial LLD).
//!
//! Set-up as `uart_init` in hal_serial_lld.c: the divider is
//! `(PCLK2 + baud / 2) / baud` in sixteenths, low nibble in DLF, the rest in
//! DLL/DLH; FIFOs on (SFE) with the RX trigger at 1 byte (SRT); interrupts on
//! received data and line status. Pins AF7, push-pull.
//!
//! The interrupt does what `serve_interrupt` does: drain RBR while LSR says
//! data ready, then, if the THR-empty interrupt is on and THR is empty,
//! refill the TX FIFO from the TX ring, or turn the interrupt off when the
//! ring is empty. ChibiOS puts one byte in per interrupt; the FIFO takes
//! up to 16 (RM2905025 25.2.7, the trigger levels), so a frame costs one
//! interrupt per 16 bytes rather than per byte. Bytes received with the RX
//! ring full are dropped and counted. RX keeps the 1-byte trigger: a higher
//! one would hold short frames (the module's ACKs) until the character
//! timeout.

use core::cell::RefCell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicU16, Ordering};
use core::task::Poll;

use critical_section::Mutex;
use embassy_sync::waitqueue::AtomicWaker;
use wb32_pac::{Interrupt, RCC, UART3, interrupt, uart};

use crate::gpio::{AnyPin, Mode, PC10, PC11};
use crate::rcc::PCLK2_HZ;

const IER_RDAIE: u32 = 1 << 0;
const IER_THREIE: u32 = 1 << 1;
const IER_RLSIE: u32 = 1 << 2;
const LCR_WLS_8BIT: u32 = 3;
const LCR_DLAB: u32 = 1 << 7;
const LSR_DR: u32 = 1 << 0;
const LSR_OE: u32 = 1 << 1;
const LSR_PE: u32 = 1 << 2;
const LSR_FE: u32 = 1 << 3;
const LSR_THRE: u32 = 1 << 5;
/// USR (DesignWare): sending or receiving; the TX FIFO empty.
const USR_BUSY: u32 = 1 << 0;
const USR_TFNF: u32 = 1 << 1;
const USR_TFE: u32 = 1 << 2;
/// The TX FIFO's depth: at most this many bytes per THR-empty interrupt.
const TX_FIFO: usize = 16;

/// Received bytes not yet read. A frame from the module is at most 36
/// bytes and is read within a millisecond or so.
const RX_LEN: usize = 128;
/// Bytes queued to send: one frame (at most 49) and an ACK or two.
const TX_LEN: usize = 128;

struct Ring<const N: usize> {
    buf: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Ring<N> {
    const fn new() -> Self {
        Ring { buf: [0; N], head: 0, len: 0 }
    }

    fn push(&mut self, b: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.buf[(self.head + self.len) % N] = b;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(b)
    }
}

static RX: Mutex<RefCell<Ring<RX_LEN>>> = Mutex::new(RefCell::new(Ring::new()));
static TX: Mutex<RefCell<Ring<TX_LEN>>> = Mutex::new(RefCell::new(Ring::new()));
static RX_WAKER: AtomicWaker = AtomicWaker::new();
/// Overrun, parity or framing errors, and bytes lost to a full RX ring.
static RX_ERRORS: AtomicU16 = AtomicU16::new(0);

/// The driver's state is its statics (the rings live with the interrupt),
/// so every method takes `&self`.
pub struct Uart3 {
    _private: (),
}

impl Uart3 {
    /// Takes UART3 and its pins; `prio` is the interrupt's (QMK: 8, `0x80`).
    pub fn new(tx: PC10, rx: PC11, baud: u32, prio: u8) -> Self {
        let mode = Mode::alternate(7);
        AnyPin::from(tx).set_mode(mode);
        AnyPin::from(rx).set_mode(mode);

        RCC.apb2enr().modify(|w| w.set_uart3en(true));
        RCC.apb2rstr().modify(|w| w.set_uart3rst(true));
        RCC.apb2rstr().modify(|w| w.set_uart3rst(false));

        let r = UART3;
        let divider = (PCLK2_HZ + baud / 2) / baud;
        r.mcr().modify(|w| w.0 &= 0x50);
        r.dlf().write_value(uart::regs::Dlf(divider & 0x0F));
        r.lcr().write_value(uart::regs::Lcr(LCR_DLAB));
        r.dll().write_value(uart::regs::Dll((divider >> 4) & 0xFF));
        r.dlh().write_value(uart::regs::Dlh((divider >> 12) & 0xFF));
        r.lcr().write_value(uart::regs::Lcr(0));
        r.lcr().write_value(uart::regs::Lcr(LCR_WLS_8BIT));
        r.srt().write_value(uart::regs::Srt(0));
        r.sfe().write_value(uart::regs::Sfe(1));
        r.ier().modify(|w| w.0 |= IER_RDAIE | IER_RLSIE);

        unsafe {
            let mut nvic = cortex_m::Peripherals::steal().NVIC;
            nvic.set_priority(Interrupt::UART3, prio);
            cortex_m::peripheral::NVIC::unpend(Interrupt::UART3);
            cortex_m::peripheral::NVIC::unmask(Interrupt::UART3);
        }
        Uart3 { _private: () }
    }

    /// Queues `bytes` to send; returns how many fit (the rest are dropped).
    pub fn write(&self, bytes: &[u8]) -> usize {
        let n = critical_section::with(|cs| {
            let mut tx = TX.borrow(cs).borrow_mut();
            bytes.iter().take_while(|&&b| tx.push(b)).count()
        });
        if n != 0 {
            // As ChibiOS's notify3: the interrupt fires at once while THR is empty.
            critical_section::with(|_| UART3.ier().modify(|w| w.0 |= IER_THREIE));
        }
        n
    }

    pub fn read(&self) -> Option<u8> {
        critical_section::with(|cs| RX.borrow(cs).borrow_mut().pop())
    }

    /// True when no received bytes are waiting.
    pub fn rx_empty(&self) -> bool {
        critical_section::with(|cs| RX.borrow(cs).borrow().len == 0)
    }

    /// Waits until a received byte is waiting.
    pub async fn wait_readable(&self) {
        poll_fn(|cx| {
            RX_WAKER.register(cx.waker());
            if self.rx_empty() { Poll::Pending } else { Poll::Ready(()) }
        })
        .await
    }

    /// Receive errors so far (line errors and RX ring overflows), saturating.
    pub fn rx_errors(&self) -> u16 {
        RX_ERRORS.load(Ordering::Relaxed)
    }
}

/// True when UART3 has nothing left to send or to hand over: both rings
/// empty, the TX FIFO empty and the UART not busy (USR, which, unlike LSR,
/// doesn't clear the error flags when read). Stop mode (`crate::stop`)
/// waits for this: its clock stops with the chip's.
pub fn idle() -> bool {
    let rings = critical_section::with(|cs| TX.borrow(cs).borrow().len == 0 && RX.borrow(cs).borrow().len == 0);
    let usr = UART3.usr().read().0;
    rings && usr & USR_TFE != 0 && usr & USR_BUSY == 0
}

/// Called in the interrupt's critical section.
fn count_error() {
    RX_ERRORS.store(RX_ERRORS.load(Ordering::Relaxed).saturating_add(1), Ordering::Relaxed);
}

fn on_interrupt() {
    let r = UART3;
    let mut lsr = r.lsr().read().0;
    let mut got = false;
    critical_section::with(|cs| {
        let mut rx = RX.borrow(cs).borrow_mut();
        while lsr & (LSR_DR | LSR_OE | LSR_PE | LSR_FE) != 0 {
            if lsr & (LSR_OE | LSR_PE | LSR_FE) != 0 {
                count_error();
            }
            let b = r.rbr().read().0 as u8;
            if lsr & LSR_DR != 0 {
                if rx.push(b) {
                    got = true;
                } else {
                    count_error();
                }
            }
            lsr = r.lsr().read().0;
        }

        if r.ier().read().0 & IER_THREIE != 0 && lsr & LSR_THRE != 0 {
            let mut tx = TX.borrow(cs).borrow_mut();
            if tx.len == 0 {
                r.ier().modify(|w| w.0 &= !IER_THREIE);
            }
            for _ in 0..TX_FIFO {
                if r.usr().read().0 & USR_TFNF == 0 {
                    break;
                }
                let Some(b) = tx.pop() else { break };
                r.thr().write_value(uart::regs::Thr(b as u32));
            }
        }
    });
    if got {
        RX_WAKER.wake();
    }
}

#[interrupt]
fn UART3() {
    on_interrupt();
}
