//! The USB device (a MUSB-style core): set-up, the interrupt, endpoint
//! helpers and the bulk transfers the command loop uses. ROM
//! 0x1FFFE3D8-0x1FFFE4D7, 0x1FFFE624-0x1FFFE7EF, 0x1FFFE7F0-0x1FFFE8AB and
//! 0x1FFFED60-0x1FFFEE33. EP0 is in ep0.rs.

use crate::clock::feed;
use crate::mmio::*;
use crate::ram::{BSS, CTRL_LEN, DATA, memset};
use wb32_pac::{GPIOA, RCC, SFM, USB};

/// ROM 0x1FFFE754-0x1FFFE7C7: clocks, the internal D+ pull-up, interrupts.
pub fn init() {
    memset(BSS.ctrl.setup.ptr() as *mut u8, CTRL_LEN, 0);
    m32(RCC.ahbenr1(), |v| v | 0x100);
    m32(RCC.ahbenr1(), |v| v | 0x02);
    m32(RCC.ahbrstr1(), |v| v | 0x02);
    m32(RCC.ahbrstr1(), |v| v & !0x02);
    w32(RCC.usbfifoclksrc(), 1);
    w32(RCC.usbfifoclkenr(), 1);
    // The internal pull-up on D+: from here the host can see the device.
    w32(SFM.usbpcon(), 2);
    w32(RCC.usbclkenr(), 1);
    // USB clock prescaler; written again with what it reads back.
    w32(RCC.usbpre(), 0x08);
    m32(RCC.usbpre(), |v| v);
    w8(USB.intrusbe(), 0);
    w8(USB.intrine(), 0);
    w8(USB.introute(), 0);
    w8(NVIC_IPR14, 0x10);
    w32(NVIC_ISER0, 1 << 14);
}

/// ROM 0x1FFFE63C-0x1FFFE66F: PA11/PA12 to AF3 (USB DM/DP), and the bus
/// reset and resume interrupts on. CFGMSK picks the pins the next writes
/// change.
pub fn pins() {
    m32(RCC.apb1enr(), |v| v | 0x8020);
    w32(GPIOA.cfgmsk(), !0x1800);
    w32(GPIOA.current(), 0x03C0_0000);
    w32(GPIOA.moder(), 0x0280_0000);
    w32(GPIOA.otyper(), 0);
    w32(GPIOA.pupdr(), 0);
    w32(GPIOA.afrh(), 0x0003_3000);
    w8(USB.intrusbe(), 0x06);
}

/// ROM 0x1FFFE6B8-0x1FFFE6EB: PA11 to input and PA12 (D+) to an open-drain
/// output driven low, so the host sees a disconnect. At start-up it's held
/// for 1.5 s; RESET and GO do it before they leave.
pub fn disconnect() {
    m32(RCC.apb1enr(), |v| v | 0x8020);
    w32(GPIOA.cfgmsk(), !0x1800);
    w32(GPIOA.moder(), 0);
    w32(GPIOA.ospeedr(), 0);
    w32(GPIOA.pupdr(), 0);
    w32(GPIOA.cfgmsk(), !0x1000);
    w32(GPIOA.moder(), 0x0100_0000);
    w32(GPIOA.otyper(), 0x1000);
    w32(GPIOA.afrh(), 0);
    w32(GPIOA.bsrr(), 0x1000_0000);
}

/// ROM 0x1FFFE678-0x1FFFE6B3: the USB interrupt and clocks off, before
/// RESET and GO. The pull-up in SFM_USBPCON is left on.
pub fn deinit() {
    w32(NVIC_ICER0, 1 << 14);
    dsb_isb();
    m32(RCC.usbpre(), |v| v & 0x08);
    w32(RCC.usbpre(), 0);
    w32(RCC.usbclkenr(), 0);
    w32(RCC.usbfifoclkenr(), 0);
    m32(RCC.ahbenr1(), |v| v & !0x02);
}

/// ROM 0x1FFFE838-0x1FFFE8A9: vector 30. All three interrupt registers are
/// read (and so cleared) first.
pub unsafe extern "C" fn irq() {
    let bus = r8(USB.intrusb());
    let tx = r8(USB.intrin());
    let rx = r8(USB.introut());
    let c = &BSS.ctrl;
    // Resume.
    if bus & 0x02 != 0 && c.state.get() == 3 {
        c.state.set(c.saved.get());
    }
    // Bus reset: EP0 only, everything forgotten. FADDR is the core's to
    // clear.
    if bus & 0x04 != 0 {
        DATA.ep0.set(0);
        w8(USB.intrine(), 0x01);
        w8(USB.introute(), 0);
        memset(c.setup.ptr() as *mut u8, CTRL_LEN, 0);
        clear_flags();
    }
    if tx & 0x01 != 0 {
        crate::ep0::ep0();
    }
    if tx & 0x02 != 0 {
        ep1_in();
    }
    if rx & 0x02 != 0 {
        ep1_out();
    }
    // Suspend.
    if bus & 0x01 != 0 && c.state.get() != 3 {
        c.saved.set(c.state.get());
        c.state.set(3);
    }
}

/// ROM 0x1FFFE7DC-0x1FFFE7E7.
fn clear_flags() {
    DATA.rx_ready.set(0);
    DATA.tx_busy.set(0);
}

/// ROM 0x1FFFE6F0-0x1FFFE6F7: the packet went.
fn ep1_in() {
    DATA.tx_busy.set(0);
}

/// ROM 0x1FFFE6FC-0x1FFFE70D: OutPktRdy means a packet waits.
fn ep1_out() {
    w8(USB.index(), 1);
    if r8(USB.outcsr1()) & 0x01 != 0 {
        DATA.rx_ready.set(1);
    }
}

/// An endpoint's FIFO, a byte at a time.
fn fifo(ep: u8) -> *mut u8 {
    USB.fifo((ep & 0x0F) as usize).as_ptr() as *mut u8
}

/// ROM 0x1FFFE440-0x1FFFE45B.
pub fn fifo_read(ep: u8, dst: *mut u8, n: u16) {
    let f = fifo(ep);
    for i in 0..n as usize {
        unsafe { dst.add(i).write(f.read_volatile()) }
    }
}

/// ROM 0x1FFFE4BC-0x1FFFE4D7.
pub fn fifo_write(ep: u8, src: *const u8, n: u16) {
    let f = fifo(ep);
    for i in 0..n as usize {
        unsafe { f.write_volatile(src.add(i).read()) }
    }
}

/// ROM 0x1FFFE3D8-0x1FFFE3F3: CLEAR_FEATURE(ENDPOINT_HALT). It resets the
/// data toggle (ClrDataTog) and doesn't touch SendStall, so a halted
/// endpoint stays halted.
pub fn clear_halt(ep: u8) {
    if ep & 0x0F != 0 {
        w8(USB.index(), ep & 0x0F);
        if ep & 0x80 == 0 {
            w8(USB.outcsr1(), 0x80);
            return;
        }
        w8(USB.incsr1(), 0x40);
    }
}

/// ROM 0x1FFFE47C-0x1FFFE497: SET_FEATURE(ENDPOINT_HALT): SendStall.
pub fn set_halt(ep: u8) {
    if ep & 0x0F != 0 {
        w8(USB.index(), ep & 0x0F);
        if ep & 0x80 == 0 {
            w8(USB.outcsr1(), 0x20);
            return;
        }
        w8(USB.incsr1(), 0x10);
    }
}

/// ROM 0x1FFFE418-0x1FFFE43B: SendStall, for GET_STATUS(endpoint).
pub fn halted(ep: u8) -> bool {
    if ep & 0x0F != 0 {
        w8(USB.index(), ep & 0x0F);
        let stall = if ep & 0x80 != 0 { r8(USB.incsr1()) & 0x10 } else { r8(USB.outcsr1()) & 0x20 };
        return stall != 0;
    }
    false
}

/// ROM 0x1FFFE3F8-0x1FFFE413: bytes in an OUT endpoint's FIFO.
fn out_count(ep: u8) -> u16 {
    if ep & 0x0F != 0 && ep & 0x80 == 0 {
        w8(USB.index(), ep & 0x0F);
        // Two byte reads (the registers are 8 bits).
        return r8(USB.outcountl()) as u16 | (r8(USB.outcounth()) as u16) << 8;
    }
    0
}

/// ROM 0x1FFFE460-0x1FFFE477: clears OutPktRdy, so the core takes the next
/// packet.
fn out_release(ep: u8) {
    if ep & 0x0F != 0 && ep & 0x80 == 0 {
        w8(USB.index(), ep & 0x0F);
        m8(USB.outcsr1(), |v| v & 0xFE);
    }
}

/// ROM 0x1FFFE49C-0x1FFFE4B7: fills an IN FIFO and sets InPktRdy.
fn in_send(ep: u8, src: *const u8, n: u16) {
    fifo_write(ep, src, n);
    if ep & 0x0F != 0 && ep & 0x80 != 0 {
        w8(USB.index(), ep & 0x0F);
        w8(USB.incsr1(), 0x01);
    }
}

/// ROM 0x1FFFE7F0-0x1FFFE833: SET_CONFIGURATION(1): EP1 IN and OUT, 64
/// bytes (MAXP counts 8-byte units), data toggles reset, FIFOs flushed
/// (twice, for double buffering) and their interrupts on. Any other
/// configuration fails.
pub fn configure(config: u8) -> bool {
    if config != 1 {
        return false;
    }
    w8(USB.index(), 1);
    w8(USB.incsr2(), 0);
    w8(USB.inmaxp(), 8);
    w8(USB.incsr1(), 0x40);
    w8(USB.incsr1(), 0x08);
    w8(USB.incsr1(), 0x08);
    m8(USB.intrine(), |v| v | 0x02);
    w8(USB.index(), 1);
    w8(USB.outcsr2(), 0);
    w8(USB.outmaxp(), 8);
    w8(USB.outcsr1(), 0x80);
    w8(USB.outcsr1(), 0x10);
    w8(USB.outcsr1(), 0x10);
    m8(USB.introute(), |v| v | 0x02);
    true
}

/// ROM 0x1FFFE624-0x1FFFE637: EP1 interrupts off.
pub fn unconfigure() {
    m8(USB.intrine(), |v| v & !0x02);
    m8(USB.introute(), |v| v & !0x02);
}

const CMD_MAX: usize = 0x110;

/// ROM 0x1FFFED60-0x1FFFEDCB: one command from EP1 OUT into BSS.cmd: 64-byte
/// packets until a short one, at most 0x110 bytes kept (what doesn't fit
/// stays in the FIFO until the packet is released). 0 if the device leaves
/// the configured
/// state, even part way through.
pub fn recv() -> usize {
    let c = &BSS.ctrl;
    let buf = BSS.cmd.ptr() as *mut u8;
    let mut got = 0;
    loop {
        feed();
        if c.state.get() != 2 {
            return 0;
        }
        cpsid();
        DATA.rx_ready.set(0);
        out_release(1);
        cpsie();
        while DATA.rx_ready.get() == 0 {
            feed();
            if c.state.get() != 2 {
                return 0;
            }
        }
        cpsid();
        let count = out_count(1);
        let n = (CMD_MAX - got).min(count as usize);
        fifo_read(1, unsafe { buf.add(got) }, n as u16);
        got += n;
        cpsie();
        if count != 64 {
            return got;
        }
    }
}

/// ROM 0x1FFFEDD8-0x1FFFEE31: `len` bytes of BSS.reply on EP1 IN, in 64-byte
/// packets, waiting for each; a zero-length packet ends a multiple of 64.
/// Gives up if the device leaves the configured state.
pub fn send(len: usize) {
    let c = &BSS.ctrl;
    let buf = BSS.reply.ptr() as *const u8;
    let mut sent = 0;
    loop {
        feed();
        if c.state.get() != 2 {
            return;
        }
        let n = (len - sent).min(64);
        cpsid();
        in_send(0x81, unsafe { buf.add(sent) }, n as u16);
        DATA.tx_busy.set(1);
        cpsie();
        while DATA.tx_busy.get() != 0 {
            feed();
            if c.state.get() != 2 {
                return;
            }
        }
        sent += n;
        if n != 64 {
            return;
        }
    }
}
