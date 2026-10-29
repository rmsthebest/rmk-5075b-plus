//! The USB device controller (Mentor MUSB, full speed) at 0x4001_4000, with
//! CRC (0x800) and SFM (0xC00) in the same window as plain registers; SFM
//! USBPCON (0xC40) is the PHY enable a host checks. A port of the device
//! half of ../../sim/peripherals/usb.py; the host half is akko-sim's.
//!
//! Device registers (USB_TypeDef in wb32fq95xx.h), all 8 bits:
//!   0x00 FADDR  0x01 POWER  0x02 INTRIN  0x04 INTROUT  0x06 INTRUSB
//!   0x07 INTRINE  0x09 INTROUTE  0x0B INTRUSBE  0x0C FRAMEL  0x0D FRAMEH
//!   0x0E INDEX, then indexed by INDEX: 0x10 INMAXP  0x11 CSR0/INCSR1
//!   0x12 INCSR2  0x13 OUTMAXP  0x14 OUTCSR1  0x15 OUTCSR2
//!   0x16 COUNT0/OUTCOUNTL  0x17 OUTCOUNTH;  0x20 + 4n FIFO n (byte access)
//! Interrupt flags clear on read. The NVIC line (USB, 14) is high while
//! INTRIN & INTRINE | INTROUT & INTROUTE | INTRUSB & INTRUSBE is non-zero.
//!
//! Semantics follow the Mentor MUSBFSFC programming model and what
//! ChibiOS-contrib's WB32 driver (known to work on the chip) relies on:
//! - A bus reset clears FADDR, INDEX, POWER.SUSEN, the endpoint state and
//!   FIFOs, and sets the enables to their reset values (INTRINE 0x0F,
//!   INTROUTE 0x0E, INTRUSBE 0x06: no suspend interrupt).
//! - EP0 status stage: the host's status token is NAKed while OutPktRdy is
//!   set. For a request without a data stage it completes once OutPktRdy is
//!   serviced; if DataEnd wasn't set, SetupEnd is raised.
//! - A FADDR write during a control transfer takes effect when its status
//!   stage ends (ChibiOS's USB_EARLY_SET_ADDRESS).
//! - MAXP registers count 8-byte units.
//! Anything a host couldn't accept goes to `log` as an error.
//!
//! The bus side (`tok_*`, `bus_reset`) is what the controller does for each
//! host token; it returns a [`Tok`].

use cortex_m3_emu::Size;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;

pub const IRQ: u32 = 14;
pub const NEP: usize = 4;
pub const SFM_USBPCON: u32 = 0xC40;

// CSR0
pub const C0_OUTPKTRDY: u8 = 0x01;
pub const C0_INPKTRDY: u8 = 0x02;
pub const C0_SENTSTALL: u8 = 0x04;
pub const C0_DATAEND: u8 = 0x08;
pub const C0_SETUPEND: u8 = 0x10;
pub const C0_SENDSTALL: u8 = 0x20;
pub const C0_SVDOUTPKTRDY: u8 = 0x40;
pub const C0_SVDSETUPEND: u8 = 0x80;
// INCSR1
const I_INPKTRDY: u8 = 0x01;
const I_FIFONE: u8 = 0x02;
const I_UNDERRUN: u8 = 0x04;
const I_FLUSH: u8 = 0x08;
const I_SENDSTALL: u8 = 0x10;
const I_SENTSTALL: u8 = 0x20;
const I_CLRTOG: u8 = 0x40;
// OUTCSR1
const O_OUTPKTRDY: u8 = 0x01;
const O_OVERRUN: u8 = 0x04;
const O_FLUSH: u8 = 0x10;
const O_SENDSTALL: u8 = 0x20;
const O_SENTSTALL: u8 = 0x40;
const O_CLRTOG: u8 = 0x80;
// INTRUSB
pub const U_SUSPEND: u8 = 0x01;
pub const U_RESUME: u8 = 0x02;
pub const U_RESET: u8 = 0x04;
pub const U_SOF: u8 = 0x08;
// POWER
pub const P_SUSEN: u8 = 0x01;
pub const P_SUSMD: u8 = 0x02;
pub const P_RESUME: u8 = 0x04;
pub const P_RSTFLG: u8 = 0x08;

/// What a token got from the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Nak,
    Stall,
    Ack,
    Timeout,
    Data(Vec<u8>),
}

/// The control-transfer stage the host is in (a FADDR write waits for it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ctl {
    #[default]
    None,
    Setup,
    Data,
    Status,
    Idle,
}

/// A line for the host's log: `error` lines are "HOST ERROR"s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbLog {
    pub error: bool,
    pub msg: String,
}

#[derive(Clone, Debug, Default)]
pub struct Musb {
    pub faddr: u8,
    faddr_pending: Option<u8>,
    pub power: u8,
    pub intrin: u8,
    pub introut: u8,
    pub intrusb: u8,
    intrine: u8,
    introute: u8,
    intrusbe: u8,
    index: u8,
    pub frame: u16,
    pub c0: u8,
    ep0_rx: VecDeque<u8>,
    ep0_tx: Vec<u8>,
    incsr1: [u8; NEP],
    incsr2: [u8; NEP],
    inmaxp: [u8; NEP],
    outcsr1: [u8; NEP],
    outcsr2: [u8; NEP],
    outmaxp: [u8; NEP],
    in_fifo: [Vec<u8>; NEP],
    out_fifo: [VecDeque<u8>; NEP],
    tog_in: [u8; NEP],
    tog_out: [u8; NEP],
    /// The host's data toggles.
    pub host_tog_in: [u8; NEP],
    pub host_tog_out: [u8; NEP],
    /// CRC and SFM, and USB offsets nothing models.
    other: BTreeMap<u32, u32>,
    /// Set by the host.
    pub ctl: Ctl,
    /// The host's step count (100 us each), set by the host.
    pub tick: u32,
    /// The step POWER.RESUME was set, for how long it's held.
    resume_tick: Option<u32>,
    /// IN endpoint -> the step it last set INPKTRDY.
    pub ready_tick: BTreeMap<u8, u32>,
    pub log: Vec<UsbLog>,
    /// Every firmware register access while `Some`, as usb.py's 0xF10
    /// trace ("USBT W i1 +0x12 <- 0x00").
    pub trace: Option<Vec<UsbAccess>>,
}

/// One firmware access to a USB register (offset below 0x400).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbAccess {
    pub write: bool,
    /// INDEX at the time.
    pub index: u8,
    pub off: u32,
    pub value: u8,
}

impl fmt::Display for UsbAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (rw, arrow) = if self.write { ("W", "<-") } else { ("R", "->") };
        write!(
            f,
            "USBT {rw} i{} +0x{:02x} {arrow} 0x{:02x}",
            self.index, self.off, self.value
        )
    }
}

impl Musb {
    pub fn new() -> Self {
        let mut d = Musb::default();
        d.dev_reset(true);
        d
    }

    fn info(&mut self, msg: String) {
        self.log.push(UsbLog { error: false, msg });
    }

    fn error(&mut self, msg: String) {
        self.log.push(UsbLog { error: true, msg });
    }

    /// Power-on or bus reset of the device state (`other` stays).
    pub fn dev_reset(&mut self, power_on: bool) {
        self.faddr = 0;
        self.faddr_pending = None;
        self.power = if power_on {
            0
        } else {
            self.power & !(P_SUSEN | P_SUSMD | P_RESUME)
        };
        self.intrin = 0;
        self.introut = 0;
        self.intrusb = 0;
        self.intrine = 0x0F;
        self.introute = 0x0E;
        self.intrusbe = 0x06;
        self.index = 0;
        self.frame = 0;
        self.c0 = 0;
        self.ep0_rx.clear();
        self.ep0_tx.clear();
        self.incsr1 = [0; NEP];
        self.incsr2 = [0; NEP];
        self.inmaxp = [0; NEP];
        self.outcsr1 = [0; NEP];
        self.outcsr2 = [0; NEP];
        self.outmaxp = [0; NEP];
        self.in_fifo = Default::default();
        self.out_fifo = Default::default();
        self.tog_in = [0; NEP];
        self.tog_out = [0; NEP];
    }

    pub fn irq(&self) -> bool {
        (self.intrin & self.intrine)
            | (self.introut & self.introute)
            | (self.intrusb & self.intrusbe)
            != 0
    }

    pub fn other(&self, off: u32) -> u32 {
        self.other.get(&off).copied().unwrap_or(0)
    }

    // ---- register access ----

    pub fn read(&mut self, off: u32, size: Size) -> u32 {
        if off >= 0x400 {
            return self.other(off);
        }
        if size != Size::Byte {
            self.error(format!(
                "{}-byte read of USB+0x{off:03x} (registers are 8 bits)",
                size.bytes()
            ));
        }
        let index = self.index;
        let value = self.reg_read(off);
        if let Some(t) = &mut self.trace {
            t.push(UsbAccess {
                write: false,
                index,
                off,
                value,
            });
        }
        value as u32
    }

    fn reg_read(&mut self, off: u32) -> u8 {
        let i = self.index as usize;
        match off {
            0x00 => return self.faddr_pending.unwrap_or(self.faddr),
            0x01 => return self.power,
            0x02 => return std::mem::take(&mut self.intrin),
            0x04 => return std::mem::take(&mut self.introut),
            0x06 => return std::mem::take(&mut self.intrusb),
            0x07 => return self.intrine,
            0x09 => return self.introute,
            0x0B => return self.intrusbe,
            0x0C => return self.frame as u8,
            0x0D => return (self.frame >> 8) as u8 & 7,
            0x0E => return self.index,
            _ => {}
        }
        if i >= NEP {
            return 0;
        }
        match off {
            0x10 => self.inmaxp[i],
            0x11 if i == 0 => self.c0,
            0x11 => {
                (self.incsr1[i] & !I_FIFONE)
                    | if self.in_fifo[i].is_empty() {
                        0
                    } else {
                        I_FIFONE
                    }
            }
            0x12 => self.incsr2[i],
            0x13 => self.outmaxp[i],
            0x14 => self.outcsr1[i],
            0x15 => self.outcsr2[i],
            0x16 => {
                (if i == 0 {
                    self.ep0_rx.len()
                } else {
                    self.out_fifo[i].len()
                }) as u8
            }
            0x17 => {
                if i == 0 {
                    0
                } else {
                    (self.out_fifo[i].len() >> 8) as u8
                }
            }
            0x20..0x30 if off % 4 == 0 => {
                let n = ((off - 0x20) / 4) as usize;
                let fifo = if n == 0 {
                    &mut self.ep0_rx
                } else {
                    &mut self.out_fifo[n]
                };
                match fifo.pop_front() {
                    Some(b) => b,
                    None => {
                        self.error(format!("firmware read empty FIFO {n}"));
                        0
                    }
                }
            }
            _ => self.other(off) as u8,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        if off >= 0x400 {
            self.other.insert(off, v);
        } else {
            if let Some(t) = &mut self.trace {
                t.push(UsbAccess {
                    write: true,
                    index: self.index,
                    off,
                    value: v as u8,
                });
            }
            self.reg_write(off, v as u8);
        }
    }

    fn reg_write(&mut self, off: u32, v: u8) {
        let i = self.index as usize;
        match off {
            0x00 => {
                if matches!(self.ctl, Ctl::Setup | Ctl::Data | Ctl::Status) {
                    self.faddr_pending = Some(v & 0x7F);
                } else {
                    self.faddr = v & 0x7F;
                }
            }
            0x01 => {
                let was = self.power;
                self.power = (self.power & (P_SUSMD | P_RSTFLG)) | (v & !(P_SUSMD | P_RSTFLG));
                if self.power & P_RESUME != 0 && was & P_RESUME == 0 {
                    self.resume_tick = Some(self.tick);
                } else if was & P_RESUME != 0
                    && self.power & P_RESUME == 0
                    && let Some(t) = self.resume_tick.take()
                {
                    let held = self.tick - t;
                    self.info(format!("remote wakeup, RESUME held {} ms", (held + 5) / 10));
                }
            }
            0x07 => self.intrine = v & 0x0F,
            0x09 => self.introute = v & 0x0E,
            0x0B => self.intrusbe = v & 0x0F,
            0x0E => self.index = v & 0x0F,
            0x02 | 0x04 | 0x06 | 0x0C | 0x0D => {} // read-only
            0x10..=0x17 => {
                if i >= NEP {
                    return;
                }
                match off {
                    0x10 => self.inmaxp[i] = v,
                    0x11 if i == 0 => self.csr0_write(v),
                    0x11 => self.incsr1_write(i, v),
                    0x12 => self.incsr2[i] = v,
                    0x13 => self.outmaxp[i] = v,
                    0x14 => self.outcsr1_write(i, v),
                    0x15 => self.outcsr2[i] = v,
                    _ => {}
                }
            }
            0x20..0x30 if off % 4 == 0 => {
                let n = ((off - 0x20) / 4) as usize;
                if n == 0 {
                    &mut self.ep0_tx
                } else {
                    &mut self.in_fifo[n]
                }
                .push(v);
            }
            _ => {
                self.other.insert(off, v as u32);
            }
        }
    }

    fn csr0_write(&mut self, v: u8) {
        if v & C0_SVDOUTPKTRDY != 0 {
            self.c0 &= !C0_OUTPKTRDY;
            self.ep0_rx.clear();
        }
        if v & C0_SVDSETUPEND != 0 {
            self.c0 &= !C0_SETUPEND;
        }
        if v & C0_SENTSTALL == 0 {
            self.c0 &= !C0_SENTSTALL;
        }
        if v & C0_SENDSTALL != 0 {
            self.c0 |= C0_SENDSTALL;
        }
        if v & C0_DATAEND != 0 {
            self.c0 |= C0_DATAEND;
        }
        if v & C0_INPKTRDY != 0 {
            if self.ep0_tx.len() > 64 {
                self.error(format!("EP0 IN packet of {} bytes", self.ep0_tx.len()));
            }
            self.c0 |= C0_INPKTRDY;
        }
    }

    fn incsr1_write(&mut self, i: usize, v: u8) {
        let mut r = self.incsr1[i];
        if v & I_FLUSH != 0 {
            self.in_fifo[i].clear();
            r &= !I_INPKTRDY;
        }
        if v & I_CLRTOG != 0 {
            self.tog_in[i] = 0;
        }
        if v & I_UNDERRUN == 0 {
            r &= !I_UNDERRUN;
        }
        if v & I_SENTSTALL == 0 {
            r &= !I_SENTSTALL;
        }
        r = (r & !I_SENDSTALL) | (v & I_SENDSTALL);
        if v & I_INPKTRDY != 0 {
            let maxp = self.inmaxp[i] as usize * 8;
            if self.in_fifo[i].len() > maxp {
                self.error(format!(
                    "EP{i} IN packet of {} bytes, INMAXP allows {maxp}",
                    self.in_fifo[i].len()
                ));
            }
            r |= I_INPKTRDY;
            self.ready_tick.insert(i as u8, self.tick);
        }
        self.incsr1[i] = r;
    }

    fn outcsr1_write(&mut self, i: usize, v: u8) {
        let mut r = self.outcsr1[i];
        if v & O_FLUSH != 0 {
            self.out_fifo[i].clear();
            r &= !O_OUTPKTRDY;
        }
        if v & O_CLRTOG != 0 {
            self.tog_out[i] = 0;
        }
        if v & O_OUTPKTRDY == 0 {
            r &= !O_OUTPKTRDY;
            self.out_fifo[i].clear();
        }
        if v & O_OVERRUN == 0 {
            r &= !O_OVERRUN;
        }
        if v & O_SENTSTALL == 0 {
            r &= !O_SENTSTALL;
        }
        r = (r & !O_SENDSTALL) | (v & O_SENDSTALL);
        self.outcsr1[i] = r;
    }

    // ---- bus side ----

    fn responds(&self, addr: u8) -> bool {
        self.faddr == addr && self.power & P_SUSMD == 0
    }

    pub fn tok_setup(&mut self, addr: u8, data: &[u8]) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        self.c0 &= !(C0_INPKTRDY | C0_DATAEND | C0_SENDSTALL);
        self.ep0_tx.clear();
        self.ep0_rx = data.iter().copied().collect();
        self.c0 |= C0_OUTPKTRDY;
        self.intrin |= 1;
        Tok::Ack
    }

    fn ep0_stall_check(&mut self) -> bool {
        if self.c0 & C0_SENDSTALL != 0 {
            self.c0 = (self.c0 & !C0_SENDSTALL) | C0_SENTSTALL;
            self.intrin |= 1;
            return true;
        }
        false
    }

    pub fn tok_in0(&mut self, addr: u8) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        if self.ep0_stall_check() {
            return Tok::Stall;
        }
        if self.c0 & C0_OUTPKTRDY != 0 || self.c0 & C0_INPKTRDY == 0 {
            return Tok::Nak;
        }
        self.c0 &= !C0_INPKTRDY;
        self.intrin |= 1;
        Tok::Data(std::mem::take(&mut self.ep0_tx))
    }

    pub fn tok_out0(&mut self, addr: u8, data: &[u8]) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        if self.ep0_stall_check() {
            return Tok::Stall;
        }
        if self.c0 & C0_OUTPKTRDY != 0 {
            return Tok::Nak;
        }
        self.ep0_rx = data.iter().copied().collect();
        self.c0 |= C0_OUTPKTRDY;
        self.intrin |= 1;
        Tok::Ack
    }

    fn status_done(&mut self) {
        self.intrin |= 1;
        if let Some(a) = self.faddr_pending.take() {
            self.faddr = a;
        }
    }

    /// Status stage of a request with no data stage or an OUT data stage.
    pub fn tok_status_in0(&mut self, addr: u8, no_data: bool) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        if self.ep0_stall_check() {
            return Tok::Stall;
        }
        if self.c0 & C0_OUTPKTRDY != 0 {
            return Tok::Nak;
        }
        if self.c0 & C0_DATAEND != 0 {
            self.c0 &= !C0_DATAEND;
        } else if no_data {
            self.c0 |= C0_SETUPEND;
            self.info("status stage ended without DataEnd (SetupEnd raised)".into());
        } else {
            return Tok::Nak;
        }
        self.status_done();
        Tok::Data(Vec::new())
    }

    /// Status stage of a control read.
    pub fn tok_status_out0(&mut self, addr: u8) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        if self.ep0_stall_check() {
            return Tok::Stall;
        }
        if self.c0 & C0_DATAEND != 0 {
            self.c0 &= !C0_DATAEND;
        } else {
            self.c0 |= C0_SETUPEND;
        }
        self.c0 &= !C0_INPKTRDY;
        self.ep0_tx.clear();
        self.status_done();
        Tok::Ack
    }

    pub fn tok_in(&mut self, addr: u8, n: usize) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        let r = self.incsr1[n];
        if r & I_SENDSTALL != 0 {
            self.incsr1[n] |= I_SENTSTALL;
            self.intrin |= 1 << n;
            return Tok::Stall;
        }
        if r & I_INPKTRDY == 0 {
            return Tok::Nak;
        }
        let data = std::mem::take(&mut self.in_fifo[n]);
        self.incsr1[n] &= !I_INPKTRDY;
        if self.tog_in[n] != self.host_tog_in[n] {
            self.error(format!(
                "EP{n} IN sent DATA{}, host expected DATA{}",
                self.tog_in[n], self.host_tog_in[n]
            ));
        }
        self.tog_in[n] ^= 1;
        self.host_tog_in[n] ^= 1;
        self.intrin |= 1 << n;
        Tok::Data(data)
    }

    pub fn tok_out(&mut self, addr: u8, n: usize, data: &[u8]) -> Tok {
        if !self.responds(addr) {
            return Tok::Timeout;
        }
        let r = self.outcsr1[n];
        if r & O_SENDSTALL != 0 {
            self.outcsr1[n] |= O_SENTSTALL;
            self.introut |= 1 << n;
            return Tok::Stall;
        }
        if r & O_OUTPKTRDY != 0 {
            return Tok::Nak;
        }
        if data.len() > self.outmaxp[n] as usize * 8 {
            self.error(format!(
                "EP{n} OUT packet of {} bytes, OUTMAXP allows {}",
                data.len(),
                self.outmaxp[n] as usize * 8
            ));
        }
        if self.tog_out[n] != self.host_tog_out[n] {
            self.error(format!(
                "EP{n} OUT expects DATA{}, host sent DATA{}",
                self.tog_out[n], self.host_tog_out[n]
            ));
        }
        self.tog_out[n] ^= 1;
        self.host_tog_out[n] ^= 1;
        self.out_fifo[n] = data.iter().copied().collect();
        self.outcsr1[n] |= O_OUTPKTRDY;
        self.introut |= 1 << n;
        Tok::Ack
    }
}
