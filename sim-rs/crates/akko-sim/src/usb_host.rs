//! The scripted USB host on the board's USB port: the host half of
//! ../../sim/peripherals/usb.py, with ../../rom/sim/peripherals/usb_rom.py's
//! additions (USBPRE 0x08 as an option, the bulk OUT command, bulk IN
//! reports) folded in.
//!
//! It attaches once D+/D- (PA11/PA12) are in their USB alternate function,
//! the USB and FIFO clocks are on, USBPRE gives 48 MHz and the PHY is
//! enabled. Then, like Linux: 100 ms debounce, reset, GET_DESCRIPTOR(device,
//! 64), reset, SET_ADDRESS, the device/config/string descriptors,
//! SET_CONFIGURATION, then HID SET_IDLE and report descriptors. After that it
//! polls every IN endpoint (interrupt ones at bInterval). It runs in 100 us
//! steps of virtual time and reports what it sees as [`UsbEvent`]s.
//!
//! usb.py's generators are async functions here: `step().await` is a
//! `yield`, and each host step polls the running task once, as usb.py calls
//! `next()` on it, so the two step through a transfer alike.

use cortex_m3_emu::TICK_HZ;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use wb32_sim::Wb32;
use wb32_sim::usb::{self, Ctl, Musb, NEP, Tok};

/// One host step, in core ticks.
pub const STEP: u64 = TICK_HZ / 10_000;
const STEPS_PER_MS: u32 = 10;
/// 500 ms of NAKs before a transfer fails.
const NAK_LIMIT: u32 = 5000;

const GPIOA_MODER: u32 = 0x00;
const RCC: u32 = 0xC00;
const RCC_USBPRE: u32 = RCC + 0x14;
const RCC_AHBENR1: u32 = RCC + 0x40;
const RCC_USBCLKENR: u32 = RCC + 0x6C;
const RCC_USBFIFOCLKENR: u32 = RCC + 0x7C;

/// Host states, as usb.py's 0xF04.
pub const DETACHED: u8 = 0;
pub const ENUMERATING: u8 = 1;
pub const CONFIGURED: u8 = 2;
pub const SUSPENDED: u8 = 3;
pub const FAILED: u8 = 4;

/// Host commands, as usb.py's 0xF00 (see its header).
pub mod cmd {
    pub const OPEN_CDC: u32 = 0x01;
    pub const CDC_BYTE: u32 = 0x02;
    pub const HID_LEDS: u32 = 0x03;
    pub const SUSPEND: u32 = 0x04;
    pub const SUSPEND_WAKEUP: u32 = 0x0104;
    pub const RESUME: u32 = 0x05;
    pub const REENUMERATE: u32 = 0x06;
    pub const UNPLUG: u32 = 0x07;
    pub const PLUG: u32 = 0x08;
    pub const OUT_PACKET: u32 = 0x09;
    pub const CLEAR_HALT: u32 = 0x0A;
    pub const ABORTED_SETUP: u32 = 0x0B;
    pub const GET_REPORT: u32 = 0x0C;
    pub const SHORT_SETUP: u32 = 0x0D;
    pub const BULK_OUT: u32 = 0x0E;
}

/// What the host saw, as usb.py logs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbEvent {
    /// "HOST: ..."
    Log(String),
    /// "HOST ERROR: ...": tests fail on these.
    Error(String),
    /// A text line from a CDC-ACM data interface.
    Cdc(String),
    /// A report from a HID IN endpoint.
    Hid { ep: u8, data: Vec<u8> },
    /// A packet from a bulk IN endpoint of another class.
    Bulk { ep: u8, data: Vec<u8> },
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

impl fmt::Display for UsbEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsbEvent::Log(m) => write!(f, "HOST: {m}"),
            UsbEvent::Error(m) => write!(f, "HOST ERROR: {m}"),
            UsbEvent::Cdc(l) => write!(f, "HOST CDC: {l}"),
            UsbEvent::Hid { ep, data } => write!(f, "HOST HID {ep}: {}", hex(data)),
            UsbEvent::Bulk { ep, data } => write!(f, "HOST BULK {ep}: {}", hex(data)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Ep {
    addr: u8,
    attrs: u8,
    maxp: u16,
    interval: u8,
}

#[derive(Clone, Debug)]
struct Iface {
    num: u8,
    class: u8,
    sub: u8,
    proto: u8,
    eps: Vec<Ep>,
}

#[derive(Default)]
struct State {
    state: u8,
    addr: u8,
    cmds: VecDeque<(u32, Vec<u8>)>,
    out_bytes: Vec<u8>,
    cdc_text: Vec<u8>,
    ifaces: Vec<Iface>,
    tick: u32,
    suspended: bool,
    wakeup_capable: bool,
    wakeup_enabled: bool,
    plugged: bool,
    events: Vec<UsbEvent>,
}

/// What a task holds: the host's state and the device's bus side.
#[derive(Clone)]
struct Cx {
    h: Rc<RefCell<State>>,
    d: Rc<RefCell<Musb>>,
}

/// A control transfer's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctrl {
    Data(Vec<u8>),
    Stall,
    Failed,
}

impl Ctrl {
    fn data(&self) -> Option<&[u8]> {
        match self {
            Ctrl::Data(d) => Some(d),
            _ => None,
        }
    }
}

/// One host step: pending once, then done (a generator's `yield`).
struct Step(bool);

impl Future for Step {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

fn step() -> Step {
    Step(false)
}

async fn wait_ms(ms: u32) {
    for _ in 0..ms * STEPS_PER_MS {
        step().await;
    }
}

type Task = Pin<Box<dyn Future<Output = ()>>>;

impl Cx {
    fn log(&self, msg: impl Into<String>) {
        self.h.borrow_mut().events.push(UsbEvent::Log(msg.into()));
    }

    fn err(&self, msg: impl Into<String>) {
        let mut h = self.h.borrow_mut();
        h.events.push(UsbEvent::Error(msg.into()));
        h.state = FAILED;
    }

    fn addr(&self) -> u8 {
        self.h.borrow().addr
    }

    fn set_ctl(&self, c: Ctl) {
        self.d.borrow_mut().ctl = c;
    }

    async fn bus_reset(&self) {
        self.log("bus reset");
        {
            let mut d = self.d.borrow_mut();
            d.dev_reset(false);
            d.power |= usb::P_RSTFLG;
            d.intrusb |= usb::U_RESET;
        }
        {
            let mut h = self.h.borrow_mut();
            h.addr = 0;
            h.wakeup_enabled = false;
        }
        wait_ms(10).await;
        self.d.borrow_mut().power &= !usb::P_RSTFLG;
        wait_ms(10).await;
    }

    /// Repeats a token while it NAKs.
    async fn retry(&self, mut tok: impl FnMut(&mut Musb, u8) -> Tok, what: &str) -> Tok {
        for _ in 0..NAK_LIMIT {
            let r = tok(&mut self.d.borrow_mut(), self.addr());
            if r != Tok::Nak {
                if r == Tok::Timeout {
                    self.err(format!("{what}: no response"));
                }
                return r;
            }
            step().await;
        }
        self.err(format!("{what}: NAKed for {} ms", NAK_LIMIT / STEPS_PER_MS));
        Tok::Timeout
    }

    #[allow(clippy::too_many_arguments)]
    async fn control(&self, what: &str, bm: u8, req: u8, value: u16, index: u16, length: u16, out: Option<Vec<u8>>) -> Ctrl {
        let [v0, v1] = value.to_le_bytes();
        let [i0, i1] = index.to_le_bytes();
        let [l0, l1] = length.to_le_bytes();
        let setup = [bm, req, v0, v1, i0, i1, l0, l1];
        self.set_ctl(Ctl::Setup);
        let r = self.d.borrow_mut().tok_setup(self.addr(), &setup);
        if r != Tok::Ack {
            self.err(format!("{what}: SETUP {r:?}"));
            self.set_ctl(Ctl::None);
            return Ctrl::Failed;
        }
        step().await;
        let mut data = Vec::new();
        let r;
        if bm & 0x80 != 0 {
            self.set_ctl(Ctl::Data);
            loop {
                match self.retry(|d, a| d.tok_in0(a), &format!("{what} data IN")).await {
                    Tok::Stall => {
                        self.log(format!("{what} -> STALL"));
                        self.set_ctl(Ctl::None);
                        return Ctrl::Stall;
                    }
                    Tok::Data(p) => {
                        let short = p.len() < 64;
                        data.extend(p);
                        if short || data.len() >= length as usize {
                            break;
                        }
                    }
                    _ => {
                        self.set_ctl(Ctl::None);
                        return Ctrl::Failed;
                    }
                }
                step().await;
            }
            step().await;
            self.set_ctl(Ctl::Status);
            r = self.retry(|d, a| d.tok_status_out0(a), &format!("{what} status OUT")).await;
        } else {
            let no_data = out.as_ref().is_none_or(|o| o.is_empty());
            if let Some(out) = out.filter(|o| !o.is_empty()) {
                self.set_ctl(Ctl::Data);
                for pkt in out.chunks(64) {
                    let r = self.retry(|d, a| d.tok_out0(a, pkt), &format!("{what} data OUT")).await;
                    if r == Tok::Stall {
                        self.log(format!("{what} -> STALL"));
                        self.set_ctl(Ctl::None);
                        return Ctrl::Stall;
                    }
                    step().await;
                }
            }
            self.set_ctl(Ctl::Status);
            r = self.retry(|d, a| d.tok_status_in0(a, no_data), &format!("{what} status IN")).await;
        }
        self.set_ctl(Ctl::Idle);
        match r {
            Tok::Stall => {
                self.log(format!("{what} -> STALL"));
                Ctrl::Stall
            }
            Tok::Timeout => Ctrl::Failed,
            _ => {
                if bm & 0x80 != 0 {
                    self.log(format!("{what} -> {} bytes: {}", data.len(), hex(&data)));
                } else {
                    self.log(format!("{what} -> ok"));
                }
                Ctrl::Data(data)
            }
        }
    }

    async fn get_descriptor(&self, what: &str, dtype: u8, idx: u8, length: u16, index: u16, bm: u8) -> Ctrl {
        self.control(&format!("GET_DESCRIPTOR({what})"), bm, 6, (dtype as u16) << 8 | idx as u16, index, length, None)
            .await
    }

    fn parse_config(&self, cfg: &[u8]) {
        let mut ifaces: Vec<Iface> = Vec::new();
        let mut k = 0;
        while k + 1 < cfg.len() {
            let (ln, ty) = (cfg[k] as usize, cfg[k + 1]);
            if ln == 0 {
                self.err("zero-length descriptor in configuration");
                return;
            }
            let b = |i: usize| cfg.get(k + i).copied().unwrap_or(0);
            if ty == 4 {
                ifaces.push(Iface { num: b(2), class: b(5), sub: b(6), proto: b(7), eps: Vec::new() });
            } else if ty == 5
                && let Some(i) = ifaces.last_mut()
            {
                i.eps.push(Ep { addr: b(2), attrs: b(3), maxp: b(4) as u16 | (b(5) as u16) << 8, interval: b(6) });
            }
            k += ln;
        }
        let mut h = self.h.borrow_mut();
        h.wakeup_capable = cfg.get(7).is_some_and(|a| a & 0x20 != 0);
        h.ifaces = ifaces;
    }

    async fn string(&self, idx: u8) {
        if idx == 0 {
            return;
        }
        let s = self.get_descriptor(&format!("string {idx}"), 3, idx, 255, 0x0409, 0x80).await;
        if let Some(s) = s.data().filter(|s| s.len() >= 2) {
            let units: Vec<u16> = (2..(s[0] as usize).saturating_sub(1))
                .step_by(2)
                .map(|k| s.get(k).copied().unwrap_or(0) as u16 | (s.get(k + 1).copied().unwrap_or(0) as u16) << 8)
                .collect();
            self.log(format!("string {idx} = '{}'", String::from_utf16_lossy(&units)));
        }
    }

    async fn enumerate_device(self) {
        self.h.borrow_mut().state = ENUMERATING;
        self.bus_reset().await;
        let dd = self.get_descriptor("device", 1, 0, 64, 0, 0x80).await;
        if dd.data().is_none_or(|d| d.len() < 8) {
            self.err("first device descriptor read failed");
            return;
        }
        self.bus_reset().await;
        let new_addr = 5;
        if self.control(&format!("SET_ADDRESS({new_addr})"), 0x00, 5, new_addr as u16, 0, 0, None).await == Ctrl::Failed {
            return;
        }
        wait_ms(2).await;
        self.h.borrow_mut().addr = new_addr;
        let dd = self.get_descriptor("device", 1, 0, 18, 0, 0x80).await;
        let Some(dd) = dd.data().filter(|d| d.len() == 18).map(<[u8]>::to_vec) else {
            self.err("device descriptor read failed at the new address");
            return;
        };
        self.log(format!(
            "device {:04x}:{:04x}, EP0 {} bytes",
            dd[8] as u16 | (dd[9] as u16) << 8,
            dd[10] as u16 | (dd[11] as u16) << 8,
            dd[7]
        ));
        let c9 = self.get_descriptor("config", 2, 0, 9, 0, 0x80).await;
        let Some(c9) = c9.data().filter(|d| d.len() == 9).map(<[u8]>::to_vec) else {
            self.err("config descriptor read failed");
            return;
        };
        let total = c9[2] as u16 | (c9[3] as u16) << 8;
        let cfg = self.get_descriptor("config", 2, 0, total, 0, 0x80).await;
        let Some(cfg) = cfg.data().filter(|d| d.len() == total as usize).map(<[u8]>::to_vec) else {
            self.err("full config descriptor read failed");
            return;
        };
        self.parse_config(&cfg);
        self.get_descriptor("string 0", 3, 0, 255, 0, 0x80).await;
        for idx in [dd[15], dd[14], dd[16]] {
            self.string(idx).await;
        }
        if self.control("SET_CONFIGURATION(1)", 0x00, 9, 1, 0, 0, None).await == Ctrl::Failed {
            return;
        }
        {
            let mut d = self.d.borrow_mut();
            d.host_tog_in = [0; NEP];
            d.host_tog_out = [0; NEP];
        }
        let ifaces = self.h.borrow().ifaces.clone();
        for i in ifaces.iter().filter(|i| i.class == 3) {
            self.control(&format!("SET_IDLE(if {})", i.num), 0x21, 0x0A, 0, i.num as u16, 0, None).await;
            self.get_descriptor(&format!("HID report, if {}", i.num), 0x22, 0, 255, i.num as u16, 0x81).await;
        }
        let summary: Vec<String> = ifaces
            .iter()
            .map(|i| {
                let eps: Vec<String> = i.eps.iter().map(|e| format!("{:02x}", e.addr)).collect();
                format!("if {} class {:02x} eps [{}]", i.num, i.class, eps.join(" "))
            })
            .collect();
        self.log(format!("configured: {}", summary.join(", ")));
        self.h.borrow_mut().state = CONFIGURED;
    }

    /// The first endpoint of class `class` in direction `dir` (0x80 IN).
    fn find_ep(&self, class: u8, dir: u8, attrs: Option<u8>, sub_proto: Option<(u8, u8)>) -> Option<(u8, Ep)> {
        let h = self.h.borrow();
        h.ifaces.iter().filter(|i| i.class == class && sub_proto.is_none_or(|sp| sp == (i.sub, i.proto))).find_map(|i| {
            i.eps
                .iter()
                .find(|e| e.addr & 0x80 == dir && attrs.is_none_or(|a| e.attrs & 3 == a))
                .map(|&e| (i.num, e))
        })
    }

    async fn run_command(self, cmd: u32, data: Vec<u8>) {
        let (op, arg) = (cmd & 0xFF, (cmd >> 8) as u8);
        match op {
            0x01 => {
                let num = self.h.borrow().ifaces.iter().find(|i| i.class == 2).map(|i| i.num);
                let Some(num) = num else {
                    self.err("no CDC interface");
                    return;
                };
                let coding = vec![0x00, 0xC2, 0x01, 0x00, 0, 0, 8];
                self.control("SET_LINE_CODING", 0x21, 0x20, 0, num as u16, 7, Some(coding)).await;
                self.control("SET_CONTROL_LINE_STATE(DTR|RTS)", 0x21, 0x22, 3, num as u16, 0, None).await;
            }
            0x02 => {
                let Some((_, ep)) = self.find_ep(0x0A, 0x00, None, None) else {
                    self.err("no CDC data OUT endpoint");
                    return;
                };
                self.retry(|d, a| d.tok_out(a, (ep.addr & 0xF) as usize, &[arg]), "CDC OUT").await;
            }
            0x03 => {
                // The boot keyboard interface (subclass 1, protocol 1).
                if let Some((_, ep)) = self.find_ep(0x03, 0x00, Some(3), Some((1, 1))) {
                    self.retry(|d, a| d.tok_out(a, (ep.addr & 0xF) as usize, &[arg]), "HID OUT").await;
                    self.log(format!("HID LEDs {arg:02x} sent on EP{} OUT", ep.addr & 0xF));
                } else {
                    let Some((num, _)) = self.find_ep(0x03, 0x80, None, Some((1, 1))) else {
                        self.err("no boot keyboard interface");
                        return;
                    };
                    self.control("SET_REPORT(output)", 0x21, 0x09, 0x0200, num as u16, 1, Some(vec![arg])).await;
                }
            }
            0x04 => {
                if arg == 1 {
                    if !self.h.borrow().wakeup_capable {
                        self.err("SET_FEATURE(DEVICE_REMOTE_WAKEUP) on a device that doesn't support it");
                        return;
                    }
                    self.control("SET_FEATURE(DEVICE_REMOTE_WAKEUP)", 0x00, 0x03, 1, 0, 0, None).await;
                    self.h.borrow_mut().wakeup_enabled = true;
                }
                self.h.borrow_mut().suspended = true;
                self.log("bus idle (suspend)");
                wait_ms(3).await;
                let susen = self.d.borrow().power & usb::P_SUSEN != 0;
                if susen {
                    let mut d = self.d.borrow_mut();
                    d.power |= usb::P_SUSMD;
                    d.intrusb |= usb::U_SUSPEND;
                    self.h.borrow_mut().state = SUSPENDED;
                } else {
                    self.log("device did not detect suspend: POWER.SUSEN is clear");
                }
            }
            0x05 => {
                self.log("resume");
                {
                    let mut d = self.d.borrow_mut();
                    if d.power & usb::P_SUSMD != 0 {
                        d.power &= !usb::P_SUSMD;
                        d.intrusb |= usb::U_RESUME;
                    }
                }
                self.h.borrow_mut().suspended = false;
                wait_ms(20).await;
                self.h.borrow_mut().state = CONFIGURED;
            }
            0x06 => self.clone().enumerate_device().await,
            0x0A => {
                // CLEAR_FEATURE(ENDPOINT_HALT) with wIndex = arg, which may name
                // an endpoint the device doesn't have (a host may send anything).
                self.control(&format!("CLEAR_FEATURE(ENDPOINT_HALT, 0x{arg:02x})"), 0x02, 0x01, 0, arg as u16, 0, None)
                    .await;
            }
            0x0B => {
                // A control read the host gives up on after its SETUP: the next
                // SETUP comes before the data stage (the device sees SetupEnd).
                let setup = [0x80, 6, 0, 2, 0, 0, 0xFF, 0];
                if self.d.borrow_mut().tok_setup(self.addr(), &setup) != Tok::Ack {
                    self.err("aborted GET_DESCRIPTOR: SETUP not ACKed");
                    return;
                }
                self.log("GET_DESCRIPTOR(config) SETUP, then abandoned");
                wait_ms(2).await;
                {
                    let mut d = self.d.borrow_mut();
                    d.c0 |= usb::C0_SETUPEND;
                    d.intrin |= 1;
                }
                let got = self.get_descriptor("device, after an aborted transfer", 1, 0, 18, 0, 0x80).await;
                if !got.data().is_some_and(|d| d.starts_with(&[18, 1])) {
                    self.err(format!("no device descriptor after an aborted transfer: {got:?}"));
                }
            }
            0x0D => {
                // A SETUP of 5 bytes (no real host sends one; the device must
                // drop it, not spin on it), then an ordinary request.
                if self.d.borrow_mut().tok_setup(self.addr(), &[0x80, 6, 0, 1, 0]) != Tok::Ack {
                    self.err("short SETUP not ACKed");
                    return;
                }
                self.log("short SETUP (5 bytes)");
                // Past the 2 s software watchdog before the next SETUP.
                wait_ms(2500).await;
                let got = self.get_descriptor("device, after a short SETUP", 1, 0, 18, 0, 0x80).await;
                if !got.data().is_some_and(|d| d.starts_with(&[18, 1])) {
                    self.err(format!("no device descriptor after a short SETUP: {got:?}"));
                }
            }
            0x0C => {
                // GET_REPORT(input, id = arg) on interface bits 16-23, 4 bytes
                // asked, as Linux's battery query.
                let num = (cmd >> 16) & 0xFF;
                self.control(&format!("GET_REPORT(input {arg})"), 0xA1, 0x01, 0x0100 | arg as u16, num as u16, 4, None)
                    .await;
            }
            0x09 => {
                let has = self.h.borrow().ifaces.iter().any(|i| i.eps.iter().any(|e| e.addr == arg));
                if !has {
                    self.err(format!("no EP{arg} OUT"));
                    return;
                }
                self.retry(|d, a| d.tok_out(a, arg as usize, &data), &format!("EP{arg} OUT")).await;
                self.log(format!("EP{arg} OUT {}", hex(&data)));
            }
            0x0E => {
                // One bulk OUT transfer, packets of maxp, a ZLP when full.
                let maxp = self.h.borrow().ifaces.iter().flat_map(|i| &i.eps).find(|e| e.addr == arg).map(|e| e.maxp);
                let Some(maxp) = maxp else {
                    self.err(format!("no EP{arg} OUT"));
                    return;
                };
                let mut packets: Vec<&[u8]> = data.chunks(maxp as usize).collect();
                if data.is_empty() || data.len() % maxp as usize == 0 {
                    packets.push(&[]);
                }
                for p in &packets {
                    self.retry(|d, a| d.tok_out(a, arg as usize, p), &format!("EP{arg} OUT")).await;
                }
                self.log(format!(
                    "EP{arg} OUT {} bytes in {} packets: {}",
                    data.len(),
                    packets.len(),
                    hex(&data[..data.len().min(16)])
                ));
            }
            _ => {}
        }
    }

    fn poll_endpoints(&self) {
        let (tick, ifaces) = {
            let h = self.h.borrow();
            (h.tick, h.ifaces.clone())
        };
        let frame_start = tick % STEPS_PER_MS == 0;
        for i in &ifaces {
            for e in i.eps.iter().filter(|e| e.addr & 0x80 != 0) {
                let n = e.addr & 0xF;
                let kind = e.attrs & 3;
                if kind == 3 && !(frame_start && (tick / STEPS_PER_MS) % (e.interval.max(1) as u32) == 0) {
                    continue;
                }
                let r = self.d.borrow_mut().tok_in(self.addr(), n as usize);
                let data = match r {
                    Tok::Nak | Tok::Timeout => continue,
                    Tok::Stall => {
                        self.err(format!("EP{n} IN stalled"));
                        continue;
                    }
                    Tok::Data(d) => d,
                    Tok::Ack => Vec::new(),
                };
                let mut h = self.h.borrow_mut();
                if i.class == 0x0A {
                    for b in data {
                        if b == 0x0A {
                            let line = String::from_utf8_lossy(&std::mem::take(&mut h.cdc_text)).into_owned();
                            h.events.push(UsbEvent::Cdc(line));
                        } else if b != 0x0D {
                            h.cdc_text.push(b);
                        }
                    }
                } else if i.class == 0x03 {
                    h.events.push(UsbEvent::Hid { ep: n, data });
                } else if kind == 2 {
                    h.events.push(UsbEvent::Bulk { ep: n, data });
                } else {
                    h.events.push(UsbEvent::Log(format!("EP{n} IN {}", hex(&data))));
                }
            }
        }
    }
}

/// The host, stepped by the board every [`STEP`].
pub struct Host {
    cx: Cx,
    task: Option<Task>,
    next_enumerate: bool,
    /// When the next step is due (core ticks).
    pub next_at: u64,
    /// Accept USBPRE 0x08 (no divider) as 48 MHz too: the ROM runs its PLL
    /// at 48 MHz, where our firmware runs 96 MHz and divides by 2 (0x09).
    pub undivided_usbpre_ok: bool,
}

impl Host {
    /// A host whose first step is one STEP after `now`, on `dev`'s bus.
    pub fn new(dev: Rc<RefCell<Musb>>, now: u64) -> Self {
        let h = State { plugged: true, ..Default::default() };
        Host {
            cx: Cx { h: Rc::new(RefCell::new(h)), d: dev },
            task: None,
            next_enumerate: false,
            next_at: now + STEP,
            undivided_usbpre_ok: false,
        }
    }

    /// A new host after a system reset, keeping `old`'s events (Renode's
    /// log runs on), so a cursor into them stays good.
    pub fn after_reset(old: &Host, now: u64) -> Self {
        old.collect_device_log();
        let h = Host::new(old.cx.d.clone(), now);
        h.cx.h.borrow_mut().events = std::mem::take(&mut old.cx.h.borrow_mut().events);
        h
    }

    /// The state, as usb.py's 0xF04.
    pub fn state(&self) -> u8 {
        self.cx.h.borrow().state
    }

    /// Steps taken (100 us each), as usb.py's 0xF08.
    pub fn steps(&self) -> u32 {
        self.cx.h.borrow().tick
    }

    /// A command, as a write to usb.py's 0xF00. Commands 0x09 and 0x0E send
    /// the bytes queued with `out_byte` so far.
    pub fn command(&mut self, v: u32) {
        let mut h = self.cx.h.borrow_mut();
        if v == cmd::UNPLUG || v == cmd::PLUG {
            h.plugged = v == cmd::PLUG;
        } else if v & 0xFF == cmd::OUT_PACKET || v & 0xFF == cmd::BULK_OUT {
            let data = std::mem::take(&mut h.out_bytes);
            h.cmds.push_back((v, data));
        } else {
            h.cmds.push_back((v, Vec::new()));
        }
    }

    /// Queues one byte for the next OUT_PACKET or BULK_OUT, as usb.py's 0xF0C.
    pub fn out_byte(&mut self, b: u8) {
        self.cx.h.borrow_mut().out_bytes.push(b);
    }

    /// The step the device last set INPKTRDY on IN endpoint `ep`, as
    /// usb.py's 0xF14.
    pub fn ready_step(&self, ep: u8) -> Option<u32> {
        self.cx.d.borrow().ready_tick.get(&ep).copied()
    }

    /// Everything seen since power-on (or the last system reset), the
    /// device's own reports included.
    pub fn events(&self) -> std::cell::Ref<'_, Vec<UsbEvent>> {
        self.collect_device_log();
        std::cell::Ref::map(self.cx.h.borrow(), |h| &h.events)
    }

    fn collect_device_log(&self) {
        let log = std::mem::take(&mut self.cx.d.borrow_mut().log);
        let mut h = self.cx.h.borrow_mut();
        for l in log {
            h.events.push(if l.error { UsbEvent::Error(l.msg) } else { UsbEvent::Log(l.msg) });
        }
    }

    fn attached(&self, chip: &mut Wb32) -> bool {
        let moder = chip.gpio.read(GPIOA_MODER);
        let pins_af = (moder >> 22) & 3 == 2 && (moder >> 24) & 3 == 2;
        let clocks = chip.sysctl.reg(RCC_AHBENR1) & 2 != 0
            && chip.sysctl.reg(RCC_USBCLKENR) & 1 != 0
            && chip.sysctl.reg(RCC_USBFIFOCLKENR) & 1 != 0;
        pins_af && clocks && self.cx.d.borrow().other(usb::SFM_USBPCON) == 2
    }

    fn detach(&mut self, why: &str) {
        self.cx.log(why);
        self.cx.d.borrow_mut().dev_reset(true);
        {
            let mut h = self.cx.h.borrow_mut();
            h.state = DETACHED;
            h.ifaces.clear();
            h.suspended = false;
            h.cdc_text.clear();
        }
        self.task = None;
        self.next_enumerate = false;
        self.cx.set_ctl(Ctl::None);
    }

    /// One 100 us step, as usb.py's host_step.
    pub fn step(&mut self, chip: &mut Wb32) {
        self.next_at += STEP;
        let cx = self.cx.clone();
        let tick = {
            let mut h = cx.h.borrow_mut();
            h.tick += 1;
            h.tick
        };
        cx.d.borrow_mut().tick = tick;
        let ms_edge = tick % STEPS_PER_MS == 0;
        let (plugged, state) = {
            let h = cx.h.borrow();
            (h.plugged, h.state)
        };
        if !plugged {
            if state != DETACHED {
                self.detach("cable unplugged");
            }
            return self.collect_device_log();
        }
        if state == DETACHED {
            if ms_edge && self.attached(chip) {
                let pre = chip.sysctl.reg(RCC_USBPRE);
                if !(pre == 0x09 || (self.undivided_usbpre_ok && pre == 0x08)) {
                    cx.err(format!("USB clock is not 48 MHz (USBPRE = 0x{pre:x})"));
                    return self.collect_device_log();
                }
                cx.log("device attached");
                cx.h.borrow_mut().state = ENUMERATING;
                self.task = Some(Box::pin(wait_ms(100)));
                self.next_enumerate = true;
            }
            return self.collect_device_log();
        }
        if ms_edge && !self.attached(chip) {
            self.detach("device detached");
            return self.collect_device_log();
        }
        let suspended = cx.h.borrow().suspended;
        if ms_edge && !suspended {
            let mut d = cx.d.borrow_mut();
            d.frame = (d.frame + 1) & 0x7FF;
            d.intrusb |= usb::U_SOF;
        }
        let power = cx.d.borrow().power;
        if suspended && power & usb::P_RESUME != 0 && power & usb::P_SUSMD != 0 {
            if !cx.h.borrow().wakeup_enabled {
                cx.err("RESUME driven without SET_FEATURE(DEVICE_REMOTE_WAKEUP)");
            }
            cx.log("device drives resume");
            cx.d.borrow_mut().power &= !usb::P_SUSMD;
            let mut h = cx.h.borrow_mut();
            h.suspended = false;
            h.state = CONFIGURED;
        }
        if let Some(task) = &mut self.task {
            if task.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready() {
                self.task = None;
                if std::mem::take(&mut self.next_enumerate) {
                    self.task = Some(Box::pin(cx.clone().enumerate_device()));
                }
            }
        } else {
            let next = {
                let mut h = cx.h.borrow_mut();
                if matches!(h.state, CONFIGURED | SUSPENDED) { h.cmds.pop_front() } else { None }
            };
            if let Some((c, data)) = next {
                self.task = Some(Box::pin(cx.clone().run_command(c, data)));
            }
        }
        let (state, suspended) = {
            let h = cx.h.borrow();
            (h.state, h.suspended)
        };
        if state == CONFIGURED && !suspended && matches!(cx.d.borrow().ctl, Ctl::None | Ctl::Idle) {
            cx.poll_endpoints();
        }
        self.collect_device_log();
    }
}
