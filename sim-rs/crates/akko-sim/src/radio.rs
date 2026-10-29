//! The board's radio module on UART3: a behavioural model of the module
//! the vendor's firmware talks to, from its side of the protocol
//! (qmk_5075b_plus/keyboards/linker/wireless/module.h and module.c; our
//! crates/qmk-wireless). The module's own firmware isn't known, so what it
//! does between frames (how long a connection or a pairing takes, when it
//! sleeps) is an assumption, set by the pub fields.
//!
//! It splits what the keyboard sends into frames (lengths by command, as
//! module.c sends them), checks each sum, and logs
//!   "<- a6 30 d6"   a frame from the keyboard
//!   "<- ack"        the keyboard ACKed a module frame
//!   "-> 5b 32 8d"   a frame the module sends
//! It ACKs every good frame (unless `ack` is off, or after `ack_delay_ms`)
//! and acts on it:
//! - A select (A6 30-35) of a device with a bond connects after
//!   `connect_ms` (5B 32); without one it stays disconnected. Selecting the
//!   connected device again keeps the link and says so again (a keyboard
//!   that reset doesn't know). USB (A6 11) ends the link.
//! - A6 52 clears the selected device's bond. A6 51 pairs: 5B 31, then
//!   after `pair_ms` connected and bonded, rejected (5B 36) or timed out
//!   (5B 33), as `pair_outcome`. A pairing must follow the device's set-up
//!   since its select (A9, the name, for BT; AD, the VID/PID, for the
//!   dongle), as md_devs_change sends them.
//! - A6 53 is answered with the battery level (5C, the board's charger),
//!   A6 70 with the version (5D). A6 64/65/66 (charging, not, done) are
//!   kept in `charge`; A6 55-58 turn its 30 min idle sleep on and off.
//! - AF 61 (raw HID in, a Vial answer) is kept in `raw_in`.
//! Reports (A1-A5, A8) while the link isn't up are errors: the firmware
//! holds them until CONNECTED.
//!
//! A test drives the other side: `host_present` (a device's host comes
//! within reach or goes), `link_drop` (the host goes away, for a while or
//! for good), `host_state` (the host behind the dongle suspends
//! or resumes, 60 00/01), `raw_out` (a raw HID packet from the host, AF
//! 60). `faults` spoils the module's next frames; with `retransmit_ms` the
//! module sends a frame the keyboard didn't ACK again.
//!
//! The first byte the keyboard sends checks the line set-up QMK uses
//! (115200 8N1 from a 96 MHz PCLK2: DLL 52, DLH 0, DLF 1, LCR 3, FIFOs on)
//! and that C10/C11 are on UART3 (AF7): otherwise the module is wired to a
//! pin that isn't sending.

use cortex_m3_emu::TICK_HZ;
use std::collections::VecDeque;
use wb32_sim::gpio::{C, Gpio, Pin};
use wb32_sim::uart::{UartLine, UartPeer};

/// UART3_TX and UART3_RX on AF7 (the datasheet's alternate-function table).
const TX: Pin = (C, 10);
const RX: Pin = (C, 11);
const AF_UART: u8 = 7;

pub const ACK: [u8; 3] = [0x61, 0x0D, 0x0A];

// Keyboard to module (MD_SND_CMD_*).
const DEVCTRL: u8 = 0xA6;
const DEVINFO: u8 = 0xA9;
const VPID: u8 = 0xAD;
const RAW: u8 = 0xAF;
const RAW_IN: u8 = 0x61;
const REPORTS: [u8; 6] = [0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA8];
// Their DEVCTRL arguments.
const SEL_USB: u8 = 0x11;
const SEL_2G4: u8 = 0x30;
const SEL_BT5: u8 = 0x35;
const PAIR: u8 = 0x51;
const CLEAN: u8 = 0x52;
const INQVOL: u8 = 0x53;
const SLEEP_BT_EN: u8 = 0x55;
const SLEEP_2G4_DIS: u8 = 0x58;
const CHARGING: u8 = 0x64;
const CHARGING_DONE: u8 = 0x66;
const FW_VERSION: u8 = 0x70;
// Module to keyboard (MD_REV_CMD_*).
pub const STATE: u8 = 0x5B;
pub const PAIRING: u8 = 0x31;
pub const CONNECTED: u8 = 0x32;
pub const DISCONNECTED: u8 = 0x33;
pub const REJECT: u8 = 0x36;
pub const BATVOL: u8 = 0x5C;
pub const VERSION: u8 = 0x5D;
pub const HOST_STATE: u8 = 0x60;
const RAW_OUT: u8 = 0x60;
const RAW_SIZE: usize = 32;

/// Device indices (module.h DEVS_*): USB, BT1-5, 2.4G.
pub const DEVS_USB: usize = 0;
pub const DEVS_2G4: usize = 6;

/// How long after the link went down a report may still arrive: one the
/// keyboard queued before it heard (a frame's ~1 ms, the ACK's 10 ms).
const REPORT_GRACE_MS: u64 = 20;
/// How often the module sends an unACKed frame before giving up.
const RETRIES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    None,
    Pairing,
    Connected,
    Disconnected,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairOutcome {
    Accept,
    Reject,
    Timeout,
}

/// What spoils one module frame on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The sum off by one.
    BadSum,
    /// The second byte never arrives.
    LostByte,
    /// Bit 0 of the second byte flipped.
    BitError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Ack,
    /// The selected device's host is reachable again.
    HostBack,
    Connect,
    PairEnd,
    Retransmit,
}

/// Frame length by command (keyboard to module); `None`: the length byte at [1], plus 3.
fn length(cmd: u8) -> Option<Option<usize>> {
    Some(Some(match cmd {
        0xA1 => 10,
        0xA2 => 16,
        0xA3 => 4,
        0xA4 | 0xA5 | 0xA6 | 0xB4 => 3,
        0xA8 => 7,
        0xA9 => 21,
        0xAB | 0xAC => return Some(None),
        0xAD => 6,
        0xAF => 36,
        0xB3 => 5,
        _ => return None,
    }))
}

fn sum(s: &[u8]) -> u8 {
    s.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

fn ms(n: u64) -> u64 {
    n * TICK_HZ / 1000
}

pub struct Radio {
    cur: Vec<u8>,
    /// ACK the keyboard's frames, after `ack_delay_ms`.
    pub ack: bool,
    pub ack_delay_ms: u64,
    /// The level it reports, in percent (the board's charger's).
    pub battery: u8,
    pub version: u8,
    pub connect_ms: u64,
    pub pair_ms: u64,
    pub pair_outcome: PairOutcome,
    /// No report for this long, connected in a mode whose sleep is on, and
    /// the module sleeps: the link goes down until a select.
    pub idle_sleep_ms: u64,
    pub retransmit_ms: Option<u64>,
    pub faults: VecDeque<Fault>,
    /// The selected device (DEVS_*), its link, and the bonds.
    pub dev: usize,
    pub link: Link,
    pub bonded: [bool; 7],
    pub asleep: bool,
    /// The 30 min sleep: BT, 2.4G.
    pub sleep_enabled: [bool; 2],
    /// The last charge state the keyboard sent (A6 64/65/66).
    pub charge: Option<u8>,
    /// Raw HID packets the keyboard sent to the host (AF 61).
    pub raw_in: Vec<Vec<u8>>,
    /// When the selected device was selected again while its link was down.
    pub selects_while_down: Vec<u64>,
    pub errors: Vec<String>,
    /// Frames received from the keyboard.
    pub frames: u32,
    /// ACKs received from the keyboard.
    pub acks: u32,
    /// The last frame from the keyboard.
    pub last: Vec<u8>,
    /// "<- ..." and "-> ..." lines.
    pub log: Vec<String>,
    /// Each device's host is out of reach (`host_present`).
    pub host_away: [bool; 7],
    setup_checked: bool,
    due: Vec<(u64, Due)>,
    /// When `link` last changed, and the last report.
    link_since: u64,
    last_report: u64,
    /// The selected device's set-up for a pairing has come.
    pair_prep: bool,
    /// A frame the keyboard hasn't ACKed, and how often it went out.
    unacked: Option<(Vec<u8>, u32)>,
}

impl Default for Radio {
    fn default() -> Self {
        Radio {
            cur: Vec::new(),
            ack: true,
            ack_delay_ms: 0,
            battery: 100,
            version: 1,
            connect_ms: 300,
            pair_ms: 2000,
            pair_outcome: PairOutcome::Accept,
            idle_sleep_ms: 30 * 60 * 1000,
            retransmit_ms: None,
            faults: VecDeque::new(),
            dev: DEVS_USB,
            link: Link::None,
            bonded: [false; 7],
            asleep: false,
            sleep_enabled: [false; 2],
            charge: None,
            raw_in: Vec::new(),
            selects_while_down: Vec::new(),
            errors: Vec::new(),
            frames: 0,
            acks: 0,
            last: Vec::new(),
            log: Vec::new(),
            setup_checked: false,
            due: Vec::new(),
            host_away: [false; 7],
            link_since: 0,
            last_report: 0,
            pair_prep: false,
            unacked: None,
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    crate::usb_host::hex(b)
}

impl Radio {
    /// The keyboard's system reset: the module is a chip of its own and
    /// keeps its state (bonds, the link); a frame half sent is lost, and the
    /// line set-up is checked again.
    pub fn reset(&mut self) {
        self.cur.clear();
        self.setup_checked = false;
        self.due.retain(|d| d.1 != Due::Ack);
    }

    /// Bytes from the module, as they are (no faults, no retransmission).
    pub fn send(&mut self, bytes: &[u8], rx: &mut VecDeque<u8>) {
        self.log.push(format!("-> {}", hex(bytes)));
        rx.extend(bytes);
    }

    /// A module frame "cmd arg sum". A state frame (5B) moves the link, so
    /// a test can still say what the module says.
    pub fn send_frame(&mut self, cmd: u8, arg: u8, now: u64, rx: &mut VecDeque<u8>) {
        if cmd == STATE {
            let link = match arg {
                PAIRING => Some(Link::Pairing),
                CONNECTED => Some(Link::Connected),
                DISCONNECTED => Some(Link::Disconnected),
                REJECT => Some(Link::Rejected),
                _ => None,
            };
            if let Some(link) = link {
                if link == Link::Connected {
                    self.bonded[self.dev] = true;
                    self.host_away[self.dev] = false;
                }
                self.set_link(link, now);
            }
        }
        self.send_module_frame(vec![cmd, arg, cmd.wrapping_add(arg)], now, rx);
    }

    /// A frame of the module's: spoilt by the next fault, if any, and kept
    /// for retransmission until the keyboard ACKs it.
    fn send_module_frame(&mut self, f: Vec<u8>, now: u64, rx: &mut VecDeque<u8>) {
        if let Some(ms_) = self.retransmit_ms {
            self.unacked = Some((f.clone(), 1));
            self.schedule(now + ms(ms_), Due::Retransmit);
        }
        let mut wire = f;
        if let Some(fault) = self.faults.pop_front() {
            match fault {
                Fault::BadSum => *wire.last_mut().unwrap() = wire.last().unwrap().wrapping_add(1),
                Fault::LostByte => {
                    wire.remove(1);
                }
                Fault::BitError => wire[1] ^= 1,
            }
            self.log.push(format!("-> {} ({fault:?})", hex(&wire)));
            rx.extend(wire);
        } else {
            self.send(&wire, rx);
        }
    }

    fn set_link(&mut self, link: Link, now: u64) {
        if link != self.link {
            self.link = link;
            self.link_since = now;
        }
    }

    fn schedule(&mut self, at: u64, what: Due) {
        self.cancel(what);
        self.due.push((at, what));
    }

    fn cancel(&mut self, what: Due) {
        if what != Due::Ack {
            self.due.retain(|d| d.1 != what);
        }
    }

    fn wireless(&self) -> bool {
        self.dev != DEVS_USB
    }

    /// The selected device's host goes away: the link drops (5B 33) and,
    /// if `back_after_ms`, the host is back that much later and the module
    /// reconnects.
    pub fn link_drop(&mut self, back_after_ms: Option<u64>, now: u64, rx: &mut VecDeque<u8>) {
        if !self.wireless() {
            self.errors
                .push("radio: test: a link drop in USB mode".to_string());
            return;
        }
        self.host_present(self.dev, false, now, rx);
        match back_after_ms {
            Some(t) => self.schedule(now + ms(t), Due::HostBack),
            None => self.cancel(Due::HostBack),
        }
    }

    /// Device `dev`'s host comes within reach (a dongle plugged in, a
    /// phone's Bluetooth on) or goes. Bonded and selected, the module
    /// connects by itself `connect_ms` on; connected, the link drops.
    pub fn host_present(&mut self, dev: usize, present: bool, now: u64, rx: &mut VecDeque<u8>) {
        self.host_away[dev] = !present;
        if dev != self.dev || !self.wireless() {
            return;
        }
        if !present {
            self.cancel(Due::Connect);
            if self.link == Link::Connected {
                self.send_frame(STATE, DISCONNECTED, now, rx);
            }
        } else if self.bonded[dev] && self.link != Link::Connected {
            self.schedule(now + ms(self.connect_ms), Due::Connect);
        }
    }

    /// The host behind the dongle suspends (false) or resumes (60 00/01).
    pub fn host_state(&mut self, resume: bool, now: u64, rx: &mut VecDeque<u8>) {
        if self.dev != DEVS_2G4 || self.link != Link::Connected {
            self.errors
                .push("radio: test: a host state without a 2.4G link".to_string());
        }
        self.send_frame(HOST_STATE, resume as u8, now, rx);
    }

    /// A raw HID packet from the host through the dongle (AF 60 20 data
    /// sum), zero-padded to 32 bytes.
    pub fn raw_out(&mut self, data: &[u8], now: u64, rx: &mut VecDeque<u8>) {
        if self.dev != DEVS_2G4 || self.link != Link::Connected {
            self.errors
                .push("radio: test: raw HID without a 2.4G link".to_string());
        }
        let mut f = vec![RAW, RAW_OUT, RAW_SIZE as u8];
        f.extend(data);
        f.resize(3 + RAW_SIZE, 0);
        f.push(sum(&f));
        self.send_module_frame(f, now, rx);
    }

    fn got_frame(&mut self, f: Vec<u8>, now: u64, rx: &mut VecDeque<u8>) {
        self.frames += 1;
        self.log.push(format!("<- {}", hex(&f)));
        let ok = match f[0] {
            0xB3 => f[4] == 0xB3u8.wrapping_add(f[1]).wrapping_sub(f[3]),
            0xB4 => f[2] == 0xB4,
            0xA9 => {
                let n = 2 + f[1] as usize;
                f.get(n) == Some(&sum(&f[..n.min(f.len())]))
            }
            _ => f[f.len() - 1] == sum(&f[..f.len() - 1]),
        };
        self.last = f.clone();
        if !ok {
            self.errors.push(format!("radio: bad sum in {}", hex(&f)));
            return;
        }
        if !self.ack {
        } else if self.ack_delay_ms != 0 {
            self.due.push((now + ms(self.ack_delay_ms), Due::Ack));
        } else {
            self.send(&ACK, rx);
        }
        match f[0] {
            DEVCTRL => self.devctrl(f[1], now, rx),
            DEVINFO if (1..=5).contains(&self.dev) => self.pair_prep = true,
            VPID if self.dev == DEVS_2G4 => self.pair_prep = true,
            RAW if f[1] == RAW_IN => self.raw_in.push(f[3..3 + RAW_SIZE].to_vec()),
            c if REPORTS.contains(&c) => {
                self.last_report = now;
                let late = now.saturating_sub(self.link_since) > ms(REPORT_GRACE_MS);
                if (self.link != Link::Connected || !self.wireless()) && late {
                    self.errors.push(format!(
                        "radio: report {} with the link {:?} (device {})",
                        hex(&f),
                        self.link,
                        self.dev
                    ));
                }
            }
            _ => {}
        }
    }

    fn devctrl(&mut self, arg: u8, now: u64, rx: &mut VecDeque<u8>) {
        match arg {
            SEL_USB => {
                self.dev = DEVS_USB;
                self.cancel(Due::Connect);
                self.cancel(Due::PairEnd);
                self.set_link(Link::None, now);
            }
            SEL_2G4..=SEL_BT5 => {
                let dev = if arg == SEL_2G4 {
                    DEVS_2G4
                } else {
                    (arg - SEL_2G4) as usize
                };
                self.asleep = false;
                self.pair_prep = false;
                if dev == self.dev && self.link == Link::Connected {
                    // Still connected: it says so (the keyboard may have
                    // reset and forgotten).
                    self.send_frame(STATE, CONNECTED, now, rx);
                    return;
                }
                if dev == self.dev {
                    self.selects_while_down.push(now);
                } else {
                    self.dev = dev;
                    self.cancel(Due::HostBack);
                    self.cancel(Due::PairEnd);
                    self.set_link(Link::Disconnected, now);
                }
                if self.bonded[dev]
                    && !self.host_away[dev]
                    && !self.due.iter().any(|d| d.1 == Due::Connect)
                {
                    self.schedule(now + ms(self.connect_ms), Due::Connect);
                }
            }
            PAIR => {
                if !self.wireless() {
                    self.errors.push("radio: PAIR in USB mode".to_string());
                    return;
                }
                if !self.pair_prep {
                    self.errors.push(format!(
                        "radio: PAIR on device {} without its set-up (A9 for BT, AB/AC/AD for the dongle)",
                        self.dev
                    ));
                }
                self.cancel(Due::Connect);
                self.send_frame(STATE, PAIRING, now, rx);
                self.schedule(now + ms(self.pair_ms), Due::PairEnd);
            }
            CLEAN => {
                self.bonded[self.dev] = false;
                self.cancel(Due::Connect);
                if self.link == Link::Connected {
                    self.set_link(Link::Disconnected, now);
                }
            }
            INQVOL => self.send_frame(BATVOL, self.battery, now, rx),
            SLEEP_BT_EN..=SLEEP_2G4_DIS => {
                let i = (arg - SLEEP_BT_EN) as usize;
                self.sleep_enabled[i / 2] = i % 2 == 0;
            }
            CHARGING..=CHARGING_DONE => self.charge = Some(arg),
            FW_VERSION => self.send_frame(VERSION, self.version, now, rx),
            _ => {}
        }
    }

    /// When the module would fall asleep: connected, idle, its sleep on.
    fn sleep_at(&self) -> Option<u64> {
        let on = match self.dev {
            DEVS_USB => false,
            DEVS_2G4 => self.sleep_enabled[1],
            _ => self.sleep_enabled[0],
        };
        (on && !self.asleep && self.link == Link::Connected)
            .then(|| self.last_report.max(self.link_since) + ms(self.idle_sleep_ms))
    }

    fn due_event(&mut self, what: Due, now: u64, rx: &mut VecDeque<u8>) {
        match what {
            Due::Ack => self.send(&ACK, rx),
            Due::HostBack => {
                self.host_away[self.dev] = false;
                if self.wireless() && self.bonded[self.dev] && self.link != Link::Connected {
                    self.send_frame(STATE, CONNECTED, now, rx);
                }
            }
            Due::Connect => {
                if self.wireless() && self.bonded[self.dev] && !self.host_away[self.dev] {
                    self.send_frame(STATE, CONNECTED, now, rx);
                }
            }
            Due::PairEnd => match self.pair_outcome {
                PairOutcome::Accept => {
                    self.send_frame(STATE, CONNECTED, now, rx);
                }
                PairOutcome::Reject => self.send_frame(STATE, REJECT, now, rx),
                PairOutcome::Timeout => self.send_frame(STATE, DISCONNECTED, now, rx),
            },
            Due::Retransmit => {
                if let Some((f, n)) = self.unacked.take()
                    && n < RETRIES
                {
                    self.log.push(format!("-> {} (again)", hex(&f)));
                    rx.extend(&f);
                    self.unacked = Some((f, n + 1));
                    self.due
                        .push((now + ms(self.retransmit_ms.unwrap_or(0)), Due::Retransmit));
                }
            }
        }
    }

    fn tx_byte(&mut self, b: u8, now: u64, rx: &mut VecDeque<u8>) {
        self.cur.push(b);
        if self.cur[0] == 0x61 {
            if self.cur.len() == 3 {
                let f = std::mem::take(&mut self.cur);
                if f == ACK {
                    self.acks += 1;
                    self.log.push("<- ack".to_string());
                    self.unacked = None;
                    self.cancel(Due::Retransmit);
                } else {
                    self.errors.push(format!("radio: bad ACK {}", hex(&f)));
                }
            }
            return;
        }
        let Some(n) = length(self.cur[0]) else {
            self.errors.push(format!("radio: stray byte 0x{b:02x}"));
            self.cur.clear();
            return;
        };
        let n = match n {
            Some(n) => n,
            None if self.cur.len() < 2 => return,
            None => self.cur[1] as usize + 3,
        };
        if self.cur.len() == n {
            let f = std::mem::take(&mut self.cur);
            self.got_frame(f, now, rx);
        }
    }

    fn next_event(&self) -> u64 {
        let due = self.due.iter().map(|d| d.0).min().unwrap_or(u64::MAX);
        due.min(self.sleep_at().unwrap_or(u64::MAX))
    }

    fn advance(&mut self, now: u64, rx: &mut VecDeque<u8>) {
        loop {
            let Some(i) = (0..self.due.len())
                .filter(|&i| self.due[i].0 <= now)
                .min_by_key(|&i| self.due[i].0)
            else {
                break;
            };
            // At its own time: what it schedules follows from there.
            let (at, what) = self.due.remove(i);
            self.due_event(what, at, rx);
        }
        if self.sleep_at().is_some_and(|t| t <= now) {
            self.asleep = true;
            self.send_frame(STATE, DISCONNECTED, now, rx);
        }
    }
}

impl UartPeer for Radio {
    fn tx(&mut self, b: u8, line: UartLine, now: u64, gpio: &Gpio, rx: &mut VecDeque<u8>) {
        if !self.setup_checked {
            self.setup_checked = true;
            for p in [TX, RX] {
                if gpio.alternate(p) != Some(AF_UART) {
                    self.errors.push(format!(
                        "uart3: {p:?} on {:?}, not UART3 (AF7)",
                        gpio.alternate(p)
                    ));
                }
            }
            let got = (line.dll, line.dlh, line.dlf, line.lcr & 0x7F, line.sfe & 1);
            if got != (52, 0, 1, 3, 1) {
                self.errors.push(format!(
                    "uart3: DLL {} DLH {} DLF {} LCR 0x{:x} SFE {}, not 115200 8N1 with FIFOs",
                    got.0, got.1, got.2, got.3, got.4
                ));
            }
        }
        self.tx_byte(b, now, rx);
    }

    fn next_event(&self) -> u64 {
        Radio::next_event(self)
    }

    fn advance(&mut self, now: u64, rx: &mut VecDeque<u8>) {
        Radio::advance(self, now, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QMK_LINE: UartLine = UartLine {
        dll: 52,
        dlh: 0,
        dlf: 1,
        lcr: 3,
        sfe: 1,
    };

    /// C10 and C11 to AF7 (MODER 2, AFRH nibbles 2 and 3), as the
    /// firmware sets them, or C11 left an input.
    fn gpio(rx_too: bool) -> Gpio {
        let mut g = Gpio::default();
        let moder = 2 << 20 | if rx_too { 2 << 22 } else { 0 };
        g.write(0x800, moder, &mut |_| {});
        g.write(0x824, 7 << 8 | 7 << 12, &mut |_| {});
        g
    }

    /// A radio past its line check, and a keyboard frame's bytes with its
    /// sum, sent at `at` ms.
    fn radio() -> Radio {
        let mut r = Radio::default();
        r.setup_checked = true;
        r
    }

    fn kb(r: &mut Radio, at: u64, f: &[u8], rx: &mut VecDeque<u8>) {
        r.advance(ms(at), rx);
        let mut f = f.to_vec();
        f.push(sum(&f));
        for b in f {
            r.tx_byte(b, ms(at), rx);
        }
    }

    /// The module's frames (ACKs left out) up to `at` ms.
    fn sent(r: &mut Radio, at: u64, rx: &mut VecDeque<u8>) -> Vec<String> {
        r.advance(ms(at), rx);
        let v: Vec<u8> = rx.drain(..).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < v.len() {
            let n = if v[i] == RAW {
                4 + v[i + 2] as usize
            } else {
                3
            };
            if v[i..i + 3] != ACK {
                out.push(hex(&v[i..i + n]));
            }
            i += n;
        }
        out
    }

    #[test]
    fn a_bonded_device_connects_an_unbonded_one_doesnt() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        kb(&mut r, 0, &[DEVCTRL, SEL_2G4 + 1], &mut rx);
        assert_eq!(sent(&mut r, 5000, &mut rx), Vec::<String>::new());
        assert_eq!(r.link, Link::Disconnected);
        r.bonded[DEVS_2G4] = true;
        kb(&mut r, 5000, &[DEVCTRL, SEL_2G4], &mut rx);
        assert_eq!(sent(&mut r, 5299, &mut rx), Vec::<String>::new());
        assert_eq!(sent(&mut r, 5300, &mut rx), ["5b 32 8d"]);
        // Selected again: still connected, and it says so.
        kb(&mut r, 6000, &[DEVCTRL, SEL_2G4], &mut rx);
        assert_eq!(sent(&mut r, 6000, &mut rx), ["5b 32 8d"]);
        kb(&mut r, 6100, &[DEVCTRL, SEL_USB], &mut rx);
        assert_eq!(r.link, Link::None);
        assert_eq!(r.errors, Vec::<String>::new());
    }

    #[test]
    fn pairing_bonds_rejects_or_times_out() {
        for (outcome, end) in [
            (PairOutcome::Accept, "5b 32 8d"),
            (PairOutcome::Reject, "5b 36 91"),
            (PairOutcome::Timeout, "5b 33 8e"),
        ] {
            let mut rx = VecDeque::new();
            let mut r = radio();
            r.pair_outcome = outcome;
            // md_devs_change(DEVS_BT1, true).
            kb(&mut r, 0, &[DEVCTRL, SEL_2G4 + 1], &mut rx);
            kb(&mut r, 1, &[DEVCTRL, CLEAN], &mut rx);
            let mut info = vec![DEVINFO, 3, b'K', b'B', b'1'];
            info.push(sum(&info));
            info.resize(21, 0);
            r.advance(ms(2), &mut rx);
            for b in info {
                r.tx_byte(b, ms(2), &mut rx);
            }
            kb(&mut r, 3, &[DEVCTRL, PAIR], &mut rx);
            assert_eq!(sent(&mut r, 3, &mut rx), ["5b 31 8c"]);
            assert_eq!(r.link, Link::Pairing);
            assert_eq!(sent(&mut r, 2003, &mut rx), [end], "{outcome:?}");
            assert_eq!(r.bonded[1], outcome == PairOutcome::Accept);
            assert_eq!(r.errors, Vec::<String>::new());
        }
    }

    #[test]
    fn a_pairing_without_the_devices_set_up_is_an_error() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        kb(&mut r, 0, &[DEVCTRL, SEL_2G4], &mut rx);
        kb(&mut r, 1, &[DEVCTRL, PAIR], &mut rx);
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        kb(&mut r, 2, &[DEVCTRL, SEL_USB], &mut rx);
        kb(&mut r, 3, &[DEVCTRL, PAIR], &mut rx);
        assert_eq!(r.errors[1], "radio: PAIR in USB mode");
    }

    #[test]
    fn reports_only_while_connected() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        r.bonded[DEVS_2G4] = true;
        kb(&mut r, 0, &[DEVCTRL, SEL_2G4], &mut rx);
        kb(&mut r, 100, &[0xA3, 0xE9, 0x00], &mut rx);
        assert_eq!(r.errors.len(), 1, "before the link: {:?}", r.errors);
        kb(&mut r, 400, &[0xA3, 0xE9, 0x00], &mut rx);
        r.link_drop(None, ms(500), &mut rx);
        // Queued before the keyboard heard: within the grace.
        kb(&mut r, 510, &[0xA3, 0x00, 0x00], &mut rx);
        assert_eq!(r.errors.len(), 1);
        kb(&mut r, 600, &[0xA3, 0x00, 0x00], &mut rx);
        assert_eq!(r.errors.len(), 2);
        // A select while the host is away doesn't connect.
        kb(&mut r, 700, &[DEVCTRL, SEL_2G4], &mut rx);
        assert_eq!(sent(&mut r, 2000, &mut rx), ["5b 32 8d", "5b 33 8e"]);
        assert_eq!(r.selects_while_down, [ms(700)]);
        r.host_present(DEVS_2G4, true, ms(2000), &mut rx);
        assert_eq!(sent(&mut r, 2300, &mut rx), ["5b 32 8d"]);
    }

    #[test]
    fn answers_and_settings() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        r.battery = 42;
        r.version = 9;
        kb(&mut r, 0, &[DEVCTRL, INQVOL], &mut rx);
        kb(&mut r, 1, &[DEVCTRL, FW_VERSION], &mut rx);
        kb(&mut r, 2, &[DEVCTRL, CHARGING], &mut rx);
        kb(&mut r, 3, &[DEVCTRL, SLEEP_BT_EN], &mut rx);
        kb(&mut r, 4, &[DEVCTRL, SLEEP_BT_EN + 2], &mut rx);
        kb(&mut r, 5, &[DEVCTRL, SLEEP_BT_EN + 1], &mut rx);
        assert_eq!(sent(&mut r, 5, &mut rx), ["5c 2a 86", "5d 09 66"]);
        assert_eq!(r.charge, Some(CHARGING));
        assert_eq!(r.sleep_enabled, [false, true]);
        let mut raw = vec![RAW, RAW_IN, 32, 8, 0xD4];
        raw.resize(35, 0);
        kb(&mut r, 6, &raw, &mut rx);
        assert_eq!(r.raw_in.len(), 1);
        assert_eq!(r.raw_in[0][..2], [8, 0xD4]);
    }

    #[test]
    fn idle_on_a_mode_with_sleep_on_the_module_sleeps_until_a_select() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        r.idle_sleep_ms = 1000;
        r.bonded[DEVS_2G4] = true;
        kb(&mut r, 0, &[DEVCTRL, SLEEP_BT_EN + 2], &mut rx);
        kb(&mut r, 1, &[DEVCTRL, SEL_2G4], &mut rx);
        kb(&mut r, 500, &[0xA3, 0xE9, 0x00], &mut rx);
        assert_eq!(sent(&mut r, 1499, &mut rx), ["5b 32 8d"]);
        assert_eq!(sent(&mut r, 1500, &mut rx), ["5b 33 8e"]);
        assert!(r.asleep);
        kb(&mut r, 3000, &[DEVCTRL, SEL_2G4], &mut rx);
        assert!(!r.asleep);
        assert_eq!(sent(&mut r, 3300, &mut rx), ["5b 32 8d"]);
    }

    #[test]
    fn faults_spoil_a_frame_and_an_unacked_one_goes_again() {
        let mut rx = VecDeque::new();
        let mut r = radio();
        r.faults
            .extend([Fault::BadSum, Fault::LostByte, Fault::BitError]);
        for _ in 0..3 {
            kb(&mut r, 0, &[DEVCTRL, INQVOL], &mut rx);
        }
        let v: Vec<u8> = rx.drain(..).collect();
        assert_eq!(
            hex(&v),
            "61 0d 0a 5c 64 c1 61 0d 0a 5c c0 61 0d 0a 5c 65 c0"
        );
        r.retransmit_ms = Some(20);
        kb(&mut r, 0, &[DEVCTRL, INQVOL], &mut rx);
        assert_eq!(
            sent(&mut r, 100, &mut rx),
            ["5c 64 c0", "5c 64 c0", "5c 64 c0"]
        );
        assert!(r.log.iter().any(|l| l.ends_with("(again)")));
        kb(&mut r, 200, &[DEVCTRL, INQVOL], &mut rx);
        for b in ACK {
            r.tx_byte(b, ms(205), &mut rx);
        }
        assert_eq!(sent(&mut r, 300, &mut rx), ["5c 64 c0"], "ACKed: once");
    }

    #[test]
    fn the_uart_pins_must_be_on_af7() {
        let mut rx = VecDeque::new();
        let mut r = Radio::default();
        r.tx(0xA6, QMK_LINE, 0, &gpio(true), &mut rx);
        assert_eq!(r.errors, Vec::<String>::new());
        let mut r = Radio::default();
        r.tx(0xA6, QMK_LINE, 0, &gpio(false), &mut rx);
        assert_eq!(r.errors, ["uart3: (2, 11) on None, not UART3 (AF7)"]);
    }
}
