//! The board's radio module on UART3. A port of the module half of
//! ../../sim/peripherals/uart3.py.
//!
//! It splits what the keyboard sends into frames (lengths by command, as
//! module.c sends them), checks each sum, and logs
//!   "<- a6 30 d6"   a frame from the keyboard
//!   "<- ack"        the keyboard ACKed a module frame
//!   "-> 5b 32 8d"   a frame the module sends
//! It ACKs every good frame (unless told not to), answers a battery inquiry
//! (A6 53) with the battery level, and, with auto-connect on, answers a
//! wireless device select (A6 30-35) with CONNECTED (5B 32). The first byte
//! the keyboard sends checks the line set-up QMK uses (115200 8N1 from a 96
//! MHz PCLK2: DLL 52, DLH 0, DLF 1, LCR 3, FIFOs on).

use cortex_m3_emu::TICK_HZ;
use std::collections::VecDeque;
use wb32_sim::uart::{UartLine, UartPeer};

pub const ACK: [u8; 3] = [0x61, 0x0D, 0x0A];

/// `flags` bits, as uart3.py's 0xE8.
pub const NO_ACKS: u32 = 1;
pub const AUTO_CONNECT: u32 = 2;

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

pub struct Radio {
    cur: Vec<u8>,
    /// Bit 0 no ACKs, bit 1 auto-connect, bits 8-15 battery %, bits 16-23
    /// the ACK delay in ms (0: at once).
    pub flags: u32,
    pub errors: Vec<String>,
    /// Frames received from the keyboard.
    pub frames: u32,
    /// ACKs received from the keyboard.
    pub acks: u32,
    /// The last frame from the keyboard.
    pub last: Vec<u8>,
    setup_checked: bool,
    /// When delayed ACKs go out.
    acks_due: VecDeque<u64>,
    /// "<- ..." and "-> ..." lines.
    pub log: Vec<String>,
}

impl Default for Radio {
    fn default() -> Self {
        Radio {
            cur: Vec::new(),
            flags: 100 << 8,
            errors: Vec::new(),
            frames: 0,
            acks: 0,
            last: Vec::new(),
            setup_checked: false,
            acks_due: VecDeque::new(),
            log: Vec::new(),
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    crate::usb_host::hex(b)
}

impl Radio {
    /// A system reset: Renode starts uart3.py again, so everything but the
    /// logs goes back to its start.
    pub fn reset(&mut self) {
        let log = std::mem::take(&mut self.log);
        let errors = std::mem::take(&mut self.errors);
        *self = Radio { log, errors, ..Radio::default() };
    }

    /// Bytes from the module, as they are.
    pub fn send(&mut self, bytes: &[u8], rx: &mut VecDeque<u8>) {
        self.log.push(format!("-> {}", hex(bytes)));
        rx.extend(bytes);
    }

    /// A module frame "cmd arg sum".
    pub fn send_frame(&mut self, cmd: u8, arg: u8, rx: &mut VecDeque<u8>) {
        self.send(&[cmd, arg, cmd.wrapping_add(arg)], rx);
    }

    fn got_frame(&mut self, f: Vec<u8>, now: u64, rx: &mut VecDeque<u8>) {
        self.frames += 1;
        self.log.push(format!("<- {}", hex(&f)));
        let sum = |s: &[u8]| s.iter().fold(0u8, |a, &b| a.wrapping_add(b));
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
        let delay = (self.flags >> 16) & 0xFF;
        if self.flags & NO_ACKS != 0 {
        } else if delay != 0 {
            self.acks_due.push_back(now + delay as u64 * TICK_HZ / 1000);
        } else {
            self.send(&ACK, rx);
        }
        if f[0] == 0xA6 {
            if f[1] == 0x53 {
                self.send_frame(0x5C, (self.flags >> 8) as u8, rx);
            } else if (0x30..=0x35).contains(&f[1]) && self.flags & AUTO_CONNECT != 0 {
                self.send_frame(0x5B, 0x32, rx);
            }
        }
    }
}

impl UartPeer for Radio {
    fn tx(&mut self, b: u8, line: UartLine, now: u64, rx: &mut VecDeque<u8>) {
        if !self.setup_checked {
            self.setup_checked = true;
            let got = (line.dll, line.dlh, line.dlf, line.lcr & 0x7F, line.sfe & 1);
            if got != (52, 0, 1, 3, 1) {
                self.errors.push(format!(
                    "uart3: DLL {} DLH {} DLF {} LCR 0x{:x} SFE {}, not 115200 8N1 with FIFOs",
                    got.0, got.1, got.2, got.3, got.4
                ));
            }
        }
        self.cur.push(b);
        if self.cur[0] == 0x61 {
            if self.cur.len() == 3 {
                let f = std::mem::take(&mut self.cur);
                if f == ACK {
                    self.acks += 1;
                    self.log.push("<- ack".to_string());
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
        self.acks_due.front().copied().unwrap_or(u64::MAX)
    }

    fn advance(&mut self, now: u64, rx: &mut VecDeque<u8>) {
        while self.acks_due.front().is_some_and(|&t| t <= now) {
            self.acks_due.pop_front();
            self.send(&ACK, rx);
        }
    }
}
