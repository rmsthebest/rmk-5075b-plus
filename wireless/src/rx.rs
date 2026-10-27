//! Frames from the module (module.c's md_receive_msg_task), with the fixed
//! tree's resync rules.
//!
//! A frame is `cmd arg sum`, or for raw HID `AF 60 len data[len] sum`, or
//! the ACK `61 0D 0A`. Bytes that can't start a frame are skipped.

use crate::frame::{RAW_LEN, sum};

/// A frame is sent back to back (~87 us a byte), so a partial frame with
/// nothing more received for this long lost a byte.
pub const FRAME_TIMEOUT_MS: u32 = 20;
const MAX_LEN: usize = RAW_LEN + 4;

/// Frame commands, module to keyboard.
pub mod cmd {
    pub const RAW: u8 = 0xAF;
    pub const RAW_OUT: u8 = 0x60;
    pub const INDICATOR: u8 = 0x5A;
    pub const DEVCTRL: u8 = 0x5B;
    pub const BATTERY: u8 = 0x5C;
    pub const FW_VERSION: u8 = 0x5D;
    pub const HOST_STATE: u8 = 0x60;
    pub const ACK: u8 = 0x61;
}

/// `cmd::DEVCTRL` arguments: the link state.
pub mod devctrl {
    pub const PAIRING: u8 = 0x31;
    pub const CONNECTED: u8 = 0x32;
    pub const DISCONNECTED: u8 = 0x33;
    pub const REJECT: u8 = 0x36;
}

/// A good frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Raw HID (VIA) from the host, through the dongle.
    Raw([u8; RAW_LEN]),
    /// The host's keyboard LEDs (bit 1 Caps Lock, as a HID LED report).
    Indicator(u8),
    /// A `devctrl::*` link state.
    DevCtrl(u8),
    /// Battery percent.
    Battery(u8),
    FwVersion(u8),
    HostState { resume: bool },
    /// Checked and ACKed but nothing for the keyboard.
    Other,
}

/// What a byte (or the silence after one) completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rx {
    /// The module ACKed our frame.
    Ack,
    /// A frame to ACK and act on.
    Frame(Event),
    BadChecksum,
    /// A partial frame thrown away: a lost byte or a bad length.
    Resync,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Parser {
    buf: [u8; MAX_LEN],
    count: usize,
    remain: usize,
    last_rx: u32,
}

impl Parser {
    pub const fn new() -> Self {
        Parser { buf: [0; MAX_LEN], count: 0, remain: 0, last_rx: 0 }
    }

    /// Call when no received bytes are waiting: drops a partial frame whose
    /// remaining bytes never came. Bytes still waiting arrived in time even
    /// if they're read late, so this mustn't be judged while any are.
    pub fn idle(&mut self, now: u32) -> Option<Rx> {
        if self.count != 0 && now.wrapping_sub(self.last_rx) > FRAME_TIMEOUT_MS {
            self.count = 0;
            return Some(Rx::Resync);
        }
        None
    }

    pub fn feed(&mut self, byte: u8, now: u32) -> Option<Rx> {
        self.last_rx = now;
        match self.count {
            0 => {
                if matches!(
                    byte,
                    cmd::RAW | cmd::INDICATOR | cmd::DEVCTRL | cmd::BATTERY | cmd::FW_VERSION | cmd::HOST_STATE | cmd::ACK
                ) {
                    self.buf[0] = byte;
                    self.count = 1;
                    self.remain = 2;
                }
                return None;
            }
            1 => {
                self.buf[1] = byte;
                self.count = 2;
                self.remain -= 1;
                return None;
            }
            2 if self.buf[..2] == [cmd::ACK, 0x0D] && byte == 0x0A => {
                self.count = 0;
                return Some(Rx::Ack);
            }
            2 if self.buf[..2] == [cmd::RAW, cmd::RAW_OUT] => {
                if byte as usize > RAW_LEN {
                    self.count = 0;
                    return Some(Rx::Resync);
                }
                self.buf[2] = byte;
                self.count = 3;
                self.remain = byte as usize + 1;
                return None;
            }
            _ => {}
        }
        if self.count >= MAX_LEN {
            self.count = 0;
            return Some(Rx::Resync);
        }
        self.buf[self.count] = byte;
        self.count += 1;
        self.remain -= 1;
        if self.remain != 0 {
            return None;
        }

        let n = core::mem::take(&mut self.count);
        let frame = &self.buf[..n];
        if sum(&frame[..n - 1]) != frame[n - 1] {
            return Some(Rx::BadChecksum);
        }
        let arg = frame[1];
        Some(Rx::Frame(match frame[0] {
            cmd::RAW if n == MAX_LEN && frame[2] as usize == RAW_LEN => {
                Event::Raw(*frame[3..].first_chunk().unwrap())
            }
            cmd::INDICATOR => Event::Indicator(arg),
            cmd::DEVCTRL => Event::DevCtrl(arg),
            cmd::BATTERY => Event::Battery(arg),
            cmd::FW_VERSION => Event::FwVersion(arg),
            cmd::HOST_STATE => Event::HostState { resume: arg == 1 },
            _ => Event::Other,
        }))
    }
}
