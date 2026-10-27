//! The parser and send queue, case for case with
//! ~/stuff/akko/tools/wls_host_test/test_parser_queue.c, then the
//! wireless.c rules the C test doesn't cover.

use std::collections::VecDeque;

use wireless::frame::{Msg, devctrl};
use wireless::rx::{self, Event};
use wireless::{Device, DeviceStrings, Link, ModuleState};

static STRINGS: DeviceStrings = DeviceStrings {
    bt_names: [b"Test BT1", b"Test BT2", b"Test BT3", b"Test BT4", b"Test BT5"],
    dongle_manufacturer: b"T\0\0\0",
    dongle_product: b"P\0\0\0",
    vid: 0x342D,
    pid: 0xE4F6,
};

const ACK: [u8; 3] = [0x61, 0x0D, 0x0A];

/// The module's side of the UART, and md_main_task's order: send, then
/// read everything waiting.
struct Bench {
    link: Link,
    now: u32,
    rx: VecDeque<u8>,
    tx: Vec<u8>,
    events: Vec<Event>,
}

impl Bench {
    fn new() -> Self {
        Bench { link: Link::new(&STRINGS), now: 0, rx: VecDeque::new(), tx: Vec::new(), events: Vec::new() }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes);
    }

    fn frame3(&mut self, a: u8, b: u8) {
        self.feed(&[a, b, a.wrapping_add(b)]);
    }

    fn task(&mut self) {
        let tx = &mut self.tx;
        self.link.poll(self.now, &mut |b: &[u8]| tx.extend_from_slice(b));
        if self.rx.is_empty() {
            self.link.rx_idle(self.now);
        }
        while let Some(byte) = self.rx.pop_front() {
            let tx = &mut self.tx;
            if let Some(e) = self.link.receive(byte, self.now, &mut |b: &[u8]| tx.extend_from_slice(b)) {
                self.events.push(e);
            }
        }
    }

    fn run(&mut self) {
        for _ in 0..10 {
            self.task();
        }
    }

    /// Bytes written since the last call, ACKs included.
    fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    /// Lets the module ACK whatever is in flight until the queue is empty.
    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut quiet = 0;
        for _ in 0..100 {
            self.task();
            let sent = self.take_tx();
            quiet = if sent.is_empty() && !self.link.busy() { quiet + 1 } else { 0 };
            if quiet == 3 {
                break;
            }
            if !sent.is_empty() {
                out.extend(&sent);
                self.feed(&ACK);
            }
            self.now += 1;
        }
        out
    }
}

#[test]
fn test_parser_queue_c_cases() {
    let mut b = Bench::new();

    // 1. connected state frame, ACKed
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    assert_eq!(b.link.state(), ModuleState::Connected);
    assert_eq!(b.take_tx(), ACK);

    // 2. battery
    b.frame3(rx::cmd::BATTERY, 57);
    b.run();
    assert_eq!(b.link.battery(), 57);

    // 3. lost byte: a battery frame without its checksum, silence, a good frame
    b.feed(&[rx::cmd::BATTERY, 11]);
    b.run();
    b.now += 25;
    b.run();
    assert_eq!(b.link.stats.rx_resyncs, 1);
    b.frame3(rx::cmd::BATTERY, 42);
    b.run();
    assert_eq!(b.link.battery(), 42);

    // 4. slow main loop: a frame whose bytes sat in the buffer must not resync
    b.feed(&[rx::cmd::BATTERY]);
    b.run();
    b.feed(&[43, rx::cmd::BATTERY + 43]);
    b.now += 100;
    b.run();
    assert_eq!(b.link.battery(), 43);
    assert_eq!(b.link.stats.rx_resyncs, 1);

    // 5. raw frame with a bogus length: no overrun, the parser recovers
    b.feed(&[rx::cmd::RAW, rx::cmd::RAW_OUT, 200]);
    b.run();
    assert_eq!(b.link.stats.rx_resyncs, 2);
    b.frame3(rx::cmd::BATTERY, 44);
    b.run();
    assert_eq!(b.link.battery(), 44);

    // 6. a good 32-byte raw frame
    let mut raw = [0u8; 36];
    raw[..3].copy_from_slice(&[rx::cmd::RAW, rx::cmd::RAW_OUT, 32]);
    for (i, x) in raw.iter_mut().enumerate().take(35).skip(3) {
        *x = i as u8;
    }
    raw[35] = raw[..35].iter().fold(0u8, |s, &x| s.wrapping_add(x));
    b.feed(&raw);
    b.run();
    let want: [u8; 32] = raw[3..35].try_into().unwrap();
    assert!(b.events.contains(&Event::Raw(want)));

    // 7. a bad checksum is counted, the next frame is fine
    b.feed(&[rx::cmd::BATTERY, 9, 0]);
    b.run();
    assert_eq!(b.link.stats.checksum_errors, 1);
    b.frame3(rx::cmd::BATTERY, 45);
    b.run();
    assert_eq!(b.link.battery(), 45);

    // 8. a disconnect is counted
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::DISCONNECTED);
    b.run();
    assert_eq!(b.link.stats.disconnects, 1);
    assert_eq!(b.link.state(), ModuleState::Disconnected);

    // 9. the ACK frees the queue
    b.take_tx();
    let kb = [0, 0, 4, 0, 0, 0, 0, 0];
    b.link.push(Msg::Keyboard(kb));
    b.run();
    let sent = b.take_tx();
    assert_eq!(sent.len(), 10);
    assert_eq!(sent[0], 0xA1);
    b.feed(&ACK);
    b.run();
    assert!(!b.link.busy());

    // 10. an unACKed key report is dropped after the retries, then the
    // latest report goes out again. (Connected here: in the C test the
    // resend is a stub call; here it goes through the link's rules.)
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    b.take_tx();
    b.link.send_keyboard(kb, b.now);
    for _ in 0..2000 {
        if !b.link.busy() {
            break;
        }
        b.task();
        b.now += 1;
    }
    assert!(!b.link.busy());
    assert_eq!(b.link.stats.retry_drops, 1);
    assert_eq!((b.link.stats.tx_pkts, b.link.stats.ack_timeouts), (42, 41), "41 sends of the dropped frame, as the C");
    let tries = b.take_tx();
    assert_eq!(tries.len(), 41 * 10);
    b.task();
    assert_eq!(b.take_tx(), [0xA1, 0, 0, 4, 0, 0, 0, 0, 0, 0xA5], "the latest report, resent");

    // 11. a queued frame behind a dropped one still goes out
    b.feed(&ACK);
    b.task();
    b.link.send_devctrl(devctrl::INQUIRE_BATTERY);
    b.task();
    assert_eq!(b.take_tx(), [0xA6, 0x53, 0xF9]);
}

/// A frame gets an ACK only if its checksum is good; ACKs get none.
#[test]
fn acks_only_good_frames() {
    let mut b = Bench::new();
    b.frame3(rx::cmd::INDICATOR, 2);
    b.feed(&[rx::cmd::INDICATOR, 2, 0]);
    b.feed(&ACK);
    b.feed(&[0x00, 0xFF, 0x13]); // not a frame start
    b.run();
    assert_eq!(b.take_tx(), ACK);
    assert_eq!(b.link.stats.checksum_errors, 1);
}

#[test]
fn indicator_only_while_connected() {
    let mut b = Bench::new();
    b.frame3(rx::cmd::INDICATOR, 2);
    b.run();
    assert_eq!(b.link.indicator(), 0);
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    assert_eq!(b.link.indicator(), 2);
    b.link.select(Device::Bt2, false);
    assert_eq!(b.link.indicator(), 0, "a new device starts disconnected");
}

/// Fixed-tree bug 1: a release made while the link is down reaches the
/// host once it's back.
#[test]
fn release_while_disconnected_is_sent_on_reconnect() {
    let mut b = Bench::new();
    b.link.select(Device::Bt1, false);
    b.drain();
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    b.take_tx();
    b.link.send_keyboard([0, 0, 4, 0, 0, 0, 0, 0], b.now);
    b.drain();

    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::DISCONNECTED);
    b.run();
    b.take_tx();
    b.link.send_keyboard([0; 8], b.now);
    let sent = b.drain();
    assert_eq!(sent, [0xA6, 0x31, 0xD7], "no report while down; BT1 re-selected");

    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.task();
    b.task();
    assert_eq!(b.take_tx(), [ACK.as_slice(), &[0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0xA1]].concat());
}

/// Fixed-tree bug 5: reports while the link is down re-select the device at
/// most every 500 ms.
#[test]
fn reselect_is_rate_limited() {
    let mut b = Bench::new();
    b.link.select(Device::Dongle, false);
    b.drain();
    let mut selects = 0;
    for _ in 0..1000 {
        b.link.send_keyboard([0; 8], b.now);
        b.link.send_consumer(0, b.now);
        selects += b.drain().chunks(3).filter(|f| *f == [0xA6, 0x30, 0xD6]).count();
        b.now += 1;
    }
    // One select when the reports start, then one per 500 ms.
    let most = b.now as usize / 500 + 1;
    assert!((most - 1..=most).contains(&selects), "{selects} selects in {} ms", b.now);
}

#[test]
fn full_queue_is_counted_and_key_state_recovers() {
    let mut b = Bench::new();
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    b.take_tx();
    for i in 0..20u8 {
        b.link.send_keyboard([0, 0, 4 + i, 0, 0, 0, 0, 0], b.now);
    }
    assert_eq!(b.link.stats.queue_full, 4);
    b.link.send_keyboard([0; 8], b.now); // lost too: the release
    let sent = b.drain();
    let frames: Vec<&[u8]> = sent.chunks(10).collect();
    assert_eq!(frames.len(), 17, "16 queued, then the latest");
    assert_eq!(frames[16], [0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0xA1]);
}

#[test]
fn system_usages() {
    let mut b = Bench::new();
    b.frame3(rx::cmd::DEVCTRL, rx::devctrl::CONNECTED);
    b.run();
    b.take_tx();
    b.link.send_system(0x82, b.now); // sleep
    b.link.send_system(0, b.now);
    let sent = b.drain();
    assert_eq!(sent, [0xA4, 0x02, 0xA6, 0xA3, 0, 0, 0xA3]);
}

#[test]
fn battery_inquiry_waits_for_an_idle_queue() {
    let mut b = Bench::new();
    b.link.send_devctrl(devctrl::FW_VERSION);
    assert!(!b.link.inquire_battery());
    b.drain();
    assert!(b.link.inquire_battery());
}
