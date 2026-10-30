//! The link to the module: the send queue with its ACK, retry and drop
//! rules (module.c's md_send_pkt_task, smsg.c), what the module reports
//! back, and wireless.c's rules for reports while the link is down.

use crate::frame::{self, Device, DeviceStrings, MAX_FRAME, Msg, devctrl};
use crate::rx::{self, Event, Parser, Rx};

/// Messages waiting for the module. Key reports are the bulk of traffic and
/// each is ACKed in about a millisecond, so this only fills while the module
/// stops answering; selecting the dongle with a reset queues 6.
pub const QUEUE_LEN: usize = 16;
/// No ACK this long (strictly more) means try again.
pub const ACK_TIMEOUT_MS: u32 = 10;
/// Tries after the first before a frame is dropped: 41 sends, about 0.45 s.
pub const RETRIES: u8 = 40;
/// While the link is down, how often reports may re-select the device.
/// The vendor sends a select with every report. The limit came from a
/// hunch, never confirmed, that a flood of selects while typing through a
/// dropout restarts the module's reconnect each time.
pub const RESELECT_MS: u32 = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModuleState {
    #[default]
    None,
    Pairing,
    Connected,
    Disconnected,
    Reject,
}

/// Link health, read over Vial like tools/wls_stats.py does on QMK (the
/// same fields in the same order). Counters stop at 0xFFFF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames written, retries included (ACKs not).
    pub tx_pkts: u16,
    pub ack_timeouts: u16,
    /// Frames given up on after [`RETRIES`].
    pub retry_drops: u16,
    /// Messages lost because the queue was full.
    pub queue_full: u16,
    pub checksum_errors: u16,
    pub rx_resyncs: u16,
    /// The module went from connected to disconnected.
    pub disconnects: u16,
    /// Key releases lost while disconnected. Kept for the layout; always 0
    /// since the latest key state is sent on reconnect.
    pub dropped_releases: u16,
}

impl Stats {
    pub fn to_le_bytes(&self) -> [u8; 16] {
        let mut out = [0; 16];
        let fields = [
            self.tx_pkts,
            self.ack_timeouts,
            self.retry_drops,
            self.queue_full,
            self.checksum_errors,
            self.rx_resyncs,
            self.disconnects,
            self.dropped_releases,
        ];
        for (chunk, f) in out.as_chunks_mut::<2>().0.iter_mut().zip(fields) {
            *chunk = f.to_le_bytes();
        }
        out
    }
}

fn inc(counter: &mut u16) {
    *counter = counter.saturating_add(1);
}

/// A fixed ring of messages; the head is the one being sent.
struct Queue {
    msgs: [Msg; QUEUE_LEN],
    head: usize,
    len: usize,
}

impl Queue {
    const fn new() -> Self {
        Queue {
            msgs: [Msg::RfTestStop; QUEUE_LEN],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, msg: Msg) -> bool {
        if self.len == QUEUE_LEN {
            return false;
        }
        self.msgs[(self.head + self.len) % QUEUE_LEN] = msg;
        self.len += 1;
        true
    }

    fn front(&self) -> Option<&Msg> {
        (self.len != 0).then(|| &self.msgs[self.head])
    }

    fn pop(&mut self) {
        if self.len != 0 {
            self.head = (self.head + 1) % QUEUE_LEN;
            self.len -= 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Send {
    /// Nothing on the wire: send the head, if any.
    Idle,
    /// The head was sent at this time and waits for its ACK.
    Busy(u32),
    /// The head's ACK timed out: count the try, then resend or drop it.
    Retry,
}

pub struct Link {
    strings: &'static DeviceStrings,
    queue: Queue,
    send: Send,
    retries: u8,
    parser: Parser,
    pub stats: Stats,
    state: ModuleState,
    indicator: u8,
    battery: u8,
    version: u8,
    device: Device,
    /// The latest keyboard report, sent again once the link is back or
    /// after one was dropped.
    keyboard: [u8; 8],
    resend_keyboard: bool,
    last_reselect: Option<u32>,
}

impl Link {
    pub const fn new(strings: &'static DeviceStrings) -> Self {
        Link {
            strings,
            queue: Queue::new(),
            send: Send::Idle,
            retries: 0,
            parser: Parser::new(),
            stats: Stats {
                tx_pkts: 0,
                ack_timeouts: 0,
                retry_drops: 0,
                queue_full: 0,
                checksum_errors: 0,
                rx_resyncs: 0,
                disconnects: 0,
                dropped_releases: 0,
            },
            state: ModuleState::None,
            indicator: 0,
            battery: 100,
            version: 0,
            device: Device::Usb,
            keyboard: [0; 8],
            resend_keyboard: false,
            last_reselect: None,
        }
    }

    pub fn state(&self) -> ModuleState {
        self.state
    }

    pub fn connected(&self) -> bool {
        self.state == ModuleState::Connected
    }

    /// The host's keyboard LEDs as the module last reported them; 0 while
    /// not connected.
    pub fn indicator(&self) -> u8 {
        if self.connected() { self.indicator } else { 0 }
    }

    /// Battery percent, from the module's last answer (100 until then).
    pub fn battery(&self) -> u8 {
        self.battery
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    pub fn device(&self) -> Device {
        self.device
    }

    /// Messages queued or in flight.
    pub fn busy(&self) -> bool {
        self.queue.len != 0
    }

    /// Queues a message for the module.
    pub fn push(&mut self, msg: Msg) {
        if !self.queue.push(msg) {
            inc(&mut self.stats.queue_full);
            if msg.is_keyboard() {
                self.resend_keyboard = true;
            }
        }
    }

    /// The sending side; call every millisecond or so. `tx` writes bytes to
    /// the module.
    pub fn poll(&mut self, now: u32, tx: &mut impl FnMut(&[u8])) {
        match self.send {
            Send::Busy(sent) => {
                if now.wrapping_sub(sent) > ACK_TIMEOUT_MS {
                    inc(&mut self.stats.ack_timeouts);
                    self.send = Send::Retry;
                }
            }
            Send::Retry => {
                self.retries += 1;
                if self.retries > RETRIES {
                    // Key reports are snapshots: a dropped one only matters if
                    // it was the last (a release, say), so send the latest.
                    if self.queue.front().is_some_and(Msg::is_keyboard) {
                        self.resend_keyboard = true;
                    }
                    self.retries = 0;
                    self.queue.pop();
                    inc(&mut self.stats.retry_drops);
                }
                self.send = Send::Idle;
            }
            Send::Idle => {
                if self.resend_keyboard && !self.busy() {
                    self.resend_keyboard = false;
                    self.send_keyboard(self.keyboard, now);
                }
                if let Some(msg) = self.queue.front() {
                    let mut buf = [0; MAX_FRAME];
                    let n = msg.encode(&mut buf);
                    if n == 0 {
                        // Nothing the module could take (a name too long).
                        self.queue.pop();
                        return;
                    }
                    tx(&buf[..n]);
                    inc(&mut self.stats.tx_pkts);
                    self.send = Send::Busy(now);
                }
            }
        }
    }

    /// Part of a frame has come (see [`rx_idle`](Self::rx_idle)).
    pub fn mid_frame(&self) -> bool {
        self.parser.mid_frame()
    }

    /// Call when no received bytes are waiting.
    pub fn rx_idle(&mut self, now: u32) {
        if let Some(Rx::Resync) = self.parser.idle(now) {
            inc(&mut self.stats.rx_resyncs);
        }
    }

    /// A byte from the module. ACKs good frames through `tx`, keeps the
    /// link state, and returns what the firmware may want (raw HID, the
    /// host's LEDs, a state change).
    pub fn receive(&mut self, byte: u8, now: u32, tx: &mut impl FnMut(&[u8])) -> Option<Event> {
        match self.parser.feed(byte, now)? {
            Rx::Ack => {
                if let Send::Busy(_) = self.send {
                    self.retries = 0;
                    self.queue.pop();
                    self.send = Send::Idle;
                }
                None
            }
            Rx::BadChecksum => {
                inc(&mut self.stats.checksum_errors);
                None
            }
            Rx::Resync => {
                inc(&mut self.stats.rx_resyncs);
                None
            }
            Rx::Frame(event) => {
                tx(&frame::ACK);
                match event {
                    Event::Indicator(leds) => self.indicator = leds,
                    Event::DevCtrl(code) => self.set_state(code, now),
                    Event::Battery(percent) => self.battery = percent,
                    Event::FwVersion(v) => self.version = v,
                    _ => {}
                }
                Some(event)
            }
        }
    }

    fn set_state(&mut self, code: u8, now: u32) {
        let new = match code {
            rx::devctrl::PAIRING => ModuleState::Pairing,
            rx::devctrl::CONNECTED => ModuleState::Connected,
            rx::devctrl::DISCONNECTED => ModuleState::Disconnected,
            rx::devctrl::REJECT => ModuleState::Reject,
            _ => return,
        };
        if new == ModuleState::Disconnected && self.state == ModuleState::Connected {
            inc(&mut self.stats.disconnects);
        }
        let was = self.state;
        self.state = new;
        // Releases made while the link was down never reached the host.
        if new == ModuleState::Connected && was != new && self.device.is_wireless() {
            self.send_keyboard(self.keyboard, now);
        }
    }

    /// Switches to `dev` (wireless_devs_change). `reset` clears its pairing
    /// and starts pairing. The firmware moves reports between USB and the
    /// module.
    pub fn select(&mut self, dev: Device, reset: bool) {
        if dev != self.device || reset {
            self.state = ModuleState::Disconnected;
            self.indicator = 0;
        }
        self.device = dev;
        let strings = self.strings;
        frame::select(dev, reset, strings, |m| self.push(m));
    }

    /// The link is down and a report wanted out: ask the module to
    /// reconnect, at most every [`RESELECT_MS`].
    fn reselect(&mut self, now: u32) {
        if self
            .last_reselect
            .is_some_and(|t| now.wrapping_sub(t) < RESELECT_MS)
        {
            return;
        }
        self.last_reselect = Some(now);
        self.select(self.device, false);
    }

    /// Sends a report while connected; otherwise asks for a reconnect and
    /// returns false.
    fn send_report(&mut self, msg: Msg, now: u32) -> bool {
        if !self.connected() {
            self.reselect(now);
            return false;
        }
        self.push(msg);
        true
    }

    /// The 8-byte keyboard report. Kept, so the latest state goes out on
    /// reconnect even if this one can't.
    pub fn send_keyboard(&mut self, report: [u8; 8], now: u32) {
        self.keyboard = report;
        self.send_report(Msg::Keyboard(report), now);
    }

    pub fn send_consumer(&mut self, usage: u16, now: u32) {
        self.send_report(Msg::Consumer(usage), now);
    }

    /// A system control usage. The module takes power down, sleep and wake
    /// (0x81-0x83) as a bitmap; anything else, the release (0) included,
    /// goes out as a consumer usage, as wireless.c sends it.
    pub fn send_system(&mut self, usage: u16, now: u32) {
        let msg = match usage {
            0x81..=0x83 => Msg::System(1 << (usage - 0x81)),
            _ => Msg::Consumer(usage),
        };
        self.send_report(msg, now);
    }

    pub fn send_mouse(&mut self, report: [u8; 5], now: u32) {
        self.send_report(Msg::Mouse(report), now);
    }

    /// Raw HID (VIA) to the host through the dongle.
    pub fn send_raw(&mut self, data: [u8; frame::RAW_LEN]) {
        self.push(Msg::Raw(data));
    }

    pub fn send_devctrl(&mut self, code: u8) {
        self.push(Msg::DevCtrl(code));
    }

    /// Asks the module for the battery level, unless it's busy. True if asked.
    pub fn inquire_battery(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        self.send_devctrl(devctrl::INQUIRE_BATTERY);
        true
    }
}
