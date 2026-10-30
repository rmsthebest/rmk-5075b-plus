//! WS2812 LED chain on SPIM2's MOSI (B15), as QMK's `ws2812_spi` driver on
//! this board (`WS2812_SPI_DRIVER SPIDM2`, `WS2812_SPI_DIVISOR 32`).
//!
//! Encoding (`frame_byte`): each WS2812 bit is 4 SPI bits at 3 MHz, 0 = 1000
//! (333 ns high), 1 = 1110 (1 us high), so a byte carries two bits; colours go
//! out G, R, B, MSB first. The frame is 4 zero bytes, 12 bytes per LED, then
//! `RESET_BYTES` zero bytes (280 us of low line: the latch). Byte for byte
//! QMK's txbuf, but made by the interrupt as it fills the FIFO, from a copy
//! of the colours (3 bytes per LED instead of the frame's 12). It walks the
//! frame with a cursor and a table (`Xfer::take`), with no division a byte:
//! SPIM2's FIFO holds 4 (RM 22.2), so at 3 MHz the interrupt comes about
//! every byte, 256 cycles at 96 MHz, for the 3.7 ms of each frame.
//!
//! Sending: interrupt driven from the TX FIFO in transmit-only mode (CR0
//! TMOD = 01, RM 22.10.1), so no dummy RX bytes to drain. The interrupt
//! comes when the FIFO is down to a quarter (TXFTLR) and fills it, so with
//! SPIM2's 4 entries about every 3 bytes, with 2 bytes in hand; it ends
//! the transfer once the last byte is queued (the frame's tail is the latch's
//! zero bytes, so the last two still shifting don't matter). QMK's driver,
//! ChibiOS-contrib's SPIv1 LLD, sends and receives instead, with the FIFO
//! half full: about an interrupt a byte, which at 4 entries kept the CPU
//! busy for most of each 3.7 ms frame (sim-rs m7_power); the set-up is
//! otherwise QMK's (`sim-rs/golden/qmk/qmk_spim2_init.txt`; sim-rs m5_lighting
//! names the differences). The interrupt runs at `prio`; QMK uses 9
//! (`0x90`), above the flash's QSPI. A late refill only stretches a low
//! phase of the line (harmless well below the 280 us latch). A frame that
//! hasn't finished after `WRITE_TIMEOUT` (it takes about 3.7 ms) is
//! abandoned, so a stuck SPIM2 drops frames instead of stopping its caller.

use core::cell::Cell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use critical_section::Mutex;
use embassy_futures::select::{Either, select};
use embassy_sync::waitqueue::AtomicWaker;
use embassy_time::{Duration, Timer};
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
/// 8-bit frames, Motorola SPI, mode 0, transmit only (TMOD 01).
const CR0_TX_ONLY: u32 = 1 << 8 | 0x07;

/// The longest a frame may take before `write` gives up on it.
const WRITE_TIMEOUT: Duration = Duration::from_millis(20);

/// Byte `i` of the frame for `rgb` (one [r, g, b] per LED; `frame_len`
/// bytes in all).
pub fn frame_byte(rgb: &[[u8; 3]], i: usize) -> u8 {
    let Some(at) = i.checked_sub(PREAMBLE_BYTES) else {
        return 0;
    };
    let (led, k) = (at / BYTES_PER_LED, at % BYTES_PER_LED);
    let Some(&[r, g, b]) = rgb.get(led) else {
        return 0;
    };
    let c = [g, r, b][k / 4];
    let pos = k % 4;
    let hi = (c >> (7 - 2 * pos)) & 1;
    let lo = (c >> (6 - 2 * pos)) & 1;
    (if hi != 0 { 0xE0 } else { 0x80 }) | (if lo != 0 { 0x0E } else { 0x08 })
}

/// The SPI byte for two WS2812 bits, the first in bit 1 (`frame_byte`).
const PAIR: [u8; 4] = [0x88, 0x8E, 0xE8, 0xEE];

/// The transfer the interrupt is working on: the colours and where in the
/// frame the next byte is.
#[derive(Clone, Copy)]
struct Xfer {
    rgb: *const [u8; 3],
    leds: usize,
    /// Preamble bytes still to send.
    pre: u8,
    /// The LED, and its byte (0-11).
    led: usize,
    k: u8,
    tx_left: usize,
}

const IDLE: Xfer = Xfer {
    rgb: core::ptr::null(),
    leds: 0,
    pre: 0,
    led: 0,
    k: 0,
    tx_left: 0,
};

impl Xfer {
    /// The next byte of the frame: `frame_byte` without its divisions.
    fn take(&mut self, rgb: &[[u8; 3]]) -> u8 {
        if self.pre != 0 {
            self.pre -= 1;
            return 0;
        }
        let Some(&[r, g, b]) = rgb.get(self.led) else {
            return 0;
        };
        let c = [g, r, b][(self.k >> 2) as usize];
        let out = PAIR[((c >> (6 - 2 * (self.k & 3))) & 3) as usize];
        self.k += 1;
        if self.k == BYTES_PER_LED as u8 {
            self.k = 0;
            self.led += 1;
        }
        out
    }
}

// Only touched inside critical sections or the SPIM2 interrupt.
unsafe impl Send for Xfer {}

static XFER: Mutex<Cell<Xfer>> = Mutex::new(Cell::new(IDLE));
static FIFO_LEN: Mutex<Cell<usize>> = Mutex::new(Cell::new(0));
static BUSY: AtomicBool = AtomicBool::new(false);
static DONE: AtomicWaker = AtomicWaker::new();

pub struct Ws2812 {
    rgb: &'static mut [[u8; 3]],
}

impl Ws2812 {
    /// Takes SPIM2 and B15. `rgb` (one per LED) holds the frame's colours;
    /// the driver keeps it so the interrupt never reads freed memory.
    pub fn new(mosi: PB15, rgb: &'static mut [[u8; 3]], prio: u8) -> Self {
        AnyPin::from(mosi).set_mode(Mode::alternate(5));

        RCC.apb2enr().modify(|w| w.set_spim2en(true));
        RCC.apb2rstr().modify(|w| w.set_spim2rst(true));
        RCC.apb2rstr().modify(|w| w.set_spim2rst(false));

        let r = SPIM2;
        r.baudr().write_value(spi::regs::Baudr(32));
        r.ser().modify(|w| w.0 |= 1);
        r.cr0().write_value(spi::regs::Cr0(CR0_TX_ONLY));
        let fifo = crate::ssi_fifo_depth(r);
        r.rxftlr().write_value(spi::regs::Rxftlr(0));
        r.ier().write_value(spi::regs::Ier(0));
        let _ = r.icr().read();
        // A probe that finds nothing would leave no room to fill (each frame
        // then ends on the timeout); SPIM2's depth is 4 (RM p.291).
        critical_section::with(|cs| FIFO_LEN.borrow(cs).set(if fifo == 1 { 4 } else { fifo }));

        crate::enable_irq(Interrupt::SPIM2, prio);
        rgb.fill([0; 3]);
        Ws2812 { rgb }
    }

    /// Sends one frame of `rgb` (one [r, g, b] per LED, as many as `new`
    /// was given; any more are ignored); returns when the last byte is out.
    pub async fn write(&mut self, rgb: impl Iterator<Item = [u8; 3]>) {
        // A write whose future was dropped may still be sending.
        Self::finished().await;
        for (to, from) in self.rgb.iter_mut().zip(rgb) {
            *to = from;
        }
        self.start();
        Self::finished().await;
    }

    /// Waits for the frame being sent, abandoning it after `WRITE_TIMEOUT`.
    async fn finished() {
        let done = poll_fn(|cx| {
            DONE.register(cx.waker());
            if BUSY.load(Ordering::Acquire) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        if let Either::Second(()) = select(done, Timer::after(WRITE_TIMEOUT)).await {
            critical_section::with(|cs| {
                SPIM2.ier().write_value(spi::regs::Ier(0));
                // Disabling empties the FIFOs, so the next frame doesn't
                // start behind this one's leftover TX bytes.
                SPIM2.spienr().write_value(spi::regs::Spienr(0));
                SPIM2.spienr().write_value(spi::regs::Spienr(1));
                XFER.borrow(cs).set(IDLE);
                BUSY.store(false, Ordering::Release);
            });
        }
    }

    fn start(&mut self) {
        let n = frame_len(self.rgb.len());
        let r = SPIM2;
        critical_section::with(|cs| {
            let rgb = self.rgb.as_ptr();
            let leds = self.rgb.len();
            XFER.borrow(cs).set(Xfer {
                rgb,
                leds,
                pre: PREAMBLE_BYTES as u8,
                led: 0,
                k: 0,
                tx_left: n,
            });
            BUSY.store(true, Ordering::Release);
            let level = (FIFO_LEN.borrow(cs).get() / 4) as u32;
            r.txftlr().write_value(spi::regs::Txftlr(level));
            r.ier().write_value(spi::regs::Ier(IT_TXE));
            r.spienr().write_value(spi::regs::Spienr(1));
        });
    }
}

/// Fills the TX FIFO; the last byte queued ends the transfer.
fn on_interrupt() {
    let r = SPIM2;
    critical_section::with(|cs| {
        let cell = XFER.borrow(cs);
        let mut x = cell.get();
        let room = FIFO_LEN
            .borrow(cs)
            .get()
            .saturating_sub(r.txflr().read().0 as usize);
        if x.tx_left == 0 {
            // A frame abandoned by `finished` with the interrupt pending.
            return;
        }
        let n = x.tx_left.min(room);
        // SAFETY: the driver's 'static colours, which `write` leaves alone
        // until the frame is out (or abandoned, with XFER idle).
        let rgb = unsafe { core::slice::from_raw_parts(x.rgb, x.leds) };
        for _ in 0..n {
            let b = x.take(rgb);
            r.dr().write_value(spi::regs::Dr(b as u32));
        }
        x.tx_left -= n;
        if x.tx_left == 0 {
            r.ier().write_value(spi::regs::Ier(0));
            BUSY.store(false, Ordering::Release);
            DONE.wake();
        }
        cell.set(x);
    });
}

#[interrupt]
fn SPIM2() {
    on_interrupt();
}
