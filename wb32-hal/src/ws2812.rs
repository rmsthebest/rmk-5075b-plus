//! WS2812 LED chain on SPIM2's MOSI (B15), as QMK's `ws2812_spi` driver on
//! this board (`WS2812_SPI_DRIVER SPIDM2`, `WS2812_SPI_DIVISOR 32`).
//!
//! Encoding (`encode`): each WS2812 bit is 4 SPI bits at 3 MHz, 0 = 1000
//! (333 ns high), 1 = 1110 (1 us high), so a byte carries two bits; colours go
//! out G, R, B, MSB first. The frame is 4 zero bytes, 12 bytes per LED, then
//! `RESET_BYTES` zero bytes (280 us of low line: the latch). Byte for byte
//! QMK's txbuf.
//!
//! Sending: interrupt driven from the TX FIFO, as ChibiOS-contrib's SPIv1
//! LLD (`spi_lld_send` and its ISR): transmit-and-receive mode, every sent
//! byte's dummy RX byte is drained, and the transfer ends when the last one
//! is in. `sim/golden/qmk_spim2_init.txt` holds QMK's register sequence:
//! the set-up below, then per frame TXFTLR, RXFTLR, IER = TXE|RXF and
//! SPIENR = 1, then IER = RXF once everything is queued and 0 at the end.
//! The interrupt runs at `prio`; QMK uses 9 (`0x90`), above the flash's QSPI.
//! A late refill only stretches a low phase of the line (harmless well
//! below the 280 us latch).

use core::cell::Cell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use critical_section::Mutex;
use embassy_sync::waitqueue::AtomicWaker;
use wb32_pac::{Interrupt, RCC, SPIM2, interrupt, spi};

use crate::gpio::{AnyPin, Mode, PB15};

const PREAMBLE_BYTES: usize = 4;
/// `1000 * WS2812_TRST_US / (2 * WS2812_TIMING)` in QMK: 280 us at 2.67 us a byte.
pub const RESET_BYTES: usize = 1000 * 280 / (2 * 1250);
pub const BYTES_PER_LED: usize = 12;

/// Frame size for `leds` LEDs.
pub const fn frame_len(leds: usize) -> usize {
    PREAMBLE_BYTES + BYTES_PER_LED * leds + RESET_BYTES
}

const IT_TXE: u32 = 1 << 0;
const IT_RXF: u32 = 1 << 4;

/// Encodes `rgb` (one [r, g, b] per LED) into `frame` (`frame_len` bytes).
pub fn encode(rgb: impl ExactSizeIterator<Item = [u8; 3]>, frame: &mut [u8]) {
    assert_eq!(frame.len(), frame_len(rgb.len()));
    frame[..PREAMBLE_BYTES].fill(0);
    for (led, out) in rgb.zip(frame[PREAMBLE_BYTES..].chunks_exact_mut(BYTES_PER_LED)) {
        let [r, g, b] = led;
        for (c, bytes) in [g, r, b].into_iter().zip(out.chunks_exact_mut(4)) {
            for (pos, byte) in bytes.iter_mut().enumerate() {
                let hi = (c >> (7 - 2 * pos)) & 1;
                let lo = (c >> (6 - 2 * pos)) & 1;
                *byte = (if hi != 0 { 0xE0 } else { 0x80 }) | (if lo != 0 { 0x0E } else { 0x08 });
            }
        }
    }
    let end = frame.len() - RESET_BYTES;
    frame[end..].fill(0);
}

/// The transfer the interrupt is working on.
struct Xfer {
    buf: *const u8,
    tx_left: usize,
    rx_left: usize,
}

// Only touched inside critical sections or the SPIM2 interrupt.
unsafe impl Send for Xfer {}

static XFER: Mutex<Cell<Xfer>> =
    Mutex::new(Cell::new(Xfer { buf: core::ptr::null(), tx_left: 0, rx_left: 0 }));
static FIFO_LEN: Mutex<Cell<usize>> = Mutex::new(Cell::new(0));
static BUSY: AtomicBool = AtomicBool::new(false);
static DONE: AtomicWaker = AtomicWaker::new();

pub struct Ws2812 {
    frame: &'static mut [u8],
}

impl Ws2812 {
    /// Takes SPIM2 and B15. `frame` (from `frame_len`) holds the encoded
    /// frame; the driver keeps it so the interrupt never reads freed memory.
    pub fn new(mosi: PB15, frame: &'static mut [u8], prio: u8) -> Self {
        AnyPin::from(mosi).set_mode(Mode::alternate(5));

        RCC.apb2enr().modify(|w| w.set_spim2en(true));
        RCC.apb2rstr().modify(|w| w.set_spim2rst(true));
        RCC.apb2rstr().modify(|w| w.set_spim2rst(false));

        let r = SPIM2;
        r.baudr().write_value(spi::regs::Baudr(32));
        r.ser().modify(|w| w.0 |= 1);
        // 8-bit frames, Motorola SPI, mode 0, transmit and receive.
        r.cr0().write_value(spi::regs::Cr0(0x07));
        // FIFO depth probe, as ChibiOS's spi_lld_start.
        let mut fifo = 1;
        while fifo < 256 {
            r.txftlr().write_value(spi::regs::Txftlr(fifo));
            if r.txftlr().read().0 != fifo {
                break;
            }
            fifo += 1;
        }
        r.txftlr().write_value(spi::regs::Txftlr(0));
        r.rxftlr().write_value(spi::regs::Rxftlr(0));
        r.ier().write_value(spi::regs::Ier(0));
        let _ = r.icr().read();
        critical_section::with(|cs| FIFO_LEN.borrow(cs).set(if fifo == 1 { 0 } else { fifo as usize }));

        unsafe {
            let mut nvic = cortex_m::Peripherals::steal().NVIC;
            nvic.set_priority(Interrupt::SPIM2, prio);
            cortex_m::peripheral::NVIC::unpend(Interrupt::SPIM2);
            cortex_m::peripheral::NVIC::unmask(Interrupt::SPIM2);
        }
        frame.fill(0);
        Ws2812 { frame }
    }

    /// Encodes and sends one frame; returns when the last byte is out.
    pub async fn write(&mut self, rgb: impl ExactSizeIterator<Item = [u8; 3]>) {
        // A write whose future was dropped may still be sending.
        poll_fn(|cx| {
            DONE.register(cx.waker());
            if BUSY.load(Ordering::Acquire) { Poll::Pending } else { Poll::Ready(()) }
        })
        .await;
        encode(rgb, self.frame);
        self.start();
        poll_fn(|cx| {
            DONE.register(cx.waker());
            if BUSY.load(Ordering::Acquire) { Poll::Pending } else { Poll::Ready(()) }
        })
        .await;
    }

    fn start(&mut self) {
        let n = self.frame.len();
        let r = SPIM2;
        critical_section::with(|cs| {
            XFER.borrow(cs).set(Xfer { buf: self.frame.as_ptr(), tx_left: n, rx_left: n });
            BUSY.store(true, Ordering::Release);
            let level = n.min(FIFO_LEN.borrow(cs).get() / 2) as u32;
            r.txftlr().write_value(spi::regs::Txftlr(level));
            r.rxftlr().write_value(spi::regs::Rxftlr(level.saturating_sub(1)));
            r.ier().modify(|w| w.0 |= IT_TXE | IT_RXF);
            r.spienr().write_value(spi::regs::Spienr(1));
        });
    }
}

/// ChibiOS's `spi_lld_serve_event_interrupt`, for a send.
fn on_interrupt() {
    let r = SPIM2;
    critical_section::with(|cs| {
        let cell = XFER.borrow(cs);
        let mut x = cell.replace(Xfer { buf: core::ptr::null(), tx_left: 0, rx_left: 0 });
        let fifo = FIFO_LEN.borrow(cs).get();
        let isr = r.isr().read().0;

        let rx_max = x.rx_left.min(r.rxflr().read().0 as usize);
        for _ in 0..rx_max {
            let _ = r.dr().read();
            x.rx_left -= 1;
        }
        if x.rx_left == 0 {
            r.ier().write_value(spi::regs::Ier(0));
            BUSY.store(false, Ordering::Release);
            DONE.wake();
        } else if x.rx_left as u32 <= r.rxftlr().read().0 {
            r.rxftlr().write_value(spi::regs::Rxftlr(x.rx_left as u32 - 1));
        }

        if isr & IT_TXE != 0 {
            let room = fifo.saturating_sub(r.txflr().read().0 as usize);
            let gap = fifo.saturating_sub(x.rx_left - x.tx_left);
            let n = x.tx_left.min(room).min(gap);
            for _ in 0..n {
                // In bounds: tx_left counts down from the frame's length.
                let b = unsafe { *x.buf };
                x.buf = unsafe { x.buf.add(1) };
                x.tx_left -= 1;
                r.dr().write_value(spi::regs::Dr(b as u32));
            }
            if x.tx_left == 0 {
                r.ier().modify(|w| w.0 &= !IT_TXE);
            }
        }
        cell.set(x);
    });
}

#[interrupt]
fn SPIM2() {
    on_interrupt();
}
