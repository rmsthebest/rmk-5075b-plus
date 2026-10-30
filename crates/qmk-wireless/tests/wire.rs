//! The encoder against QMK's bytes. Both files come from
//! tools/wls_host_test/run.sh, which runs the fixed QMK tree's module.c on
//! the host:
//! - wire_expected.txt (dump_wire.c): every md_send_* with test bytes, then
//!   md_devs_change for each device, without and with reset, with the stub
//!   strings ("Test", 4-byte wchar_t, so the dongle product is too long and
//!   module.c drops it);
//! - wire_board.txt (dump_board.c): the md_devs_change part with this
//!   board's strings and ARM's 2-byte wchar_t.

use qmk_wireless::Device;
use qmk_wireless::frame::{self, DeviceStrings, MAX_FRAME, Msg, utf16z};

fn hex(msg: &Msg) -> Option<String> {
    let mut buf = [0; MAX_FRAME];
    let n = msg.encode(&mut buf);
    (n != 0).then(|| buf[..n].iter().map(|b| format!("{b:02x}")).collect())
}

fn selects(strings: &DeviceStrings) -> Vec<Msg> {
    let mut out = Vec::new();
    for dev in 0..=6 {
        let dev = Device::from_u8(dev).unwrap();
        frame::select(dev, false, strings, |m| out.push(m));
        frame::select(dev, true, strings, |m| out.push(m));
    }
    out
}

fn check(msgs: &[Msg], expected: &str) {
    let got: Vec<String> = msgs.iter().filter_map(hex).collect();
    let want: Vec<&str> = expected.lines().collect();
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(
            g,
            w,
            "packet {i} ({:?})",
            msgs.iter().filter(|m| hex(m).is_some()).nth(i)
        );
    }
    assert_eq!(got.len(), want.len());
}

fn utf32z(s: &str) -> &'static [u8] {
    let mut v: Vec<u8> = s.bytes().flat_map(|b| [b, 0, 0, 0]).collect();
    v.extend([0; 4]);
    v.leak()
}

#[test]
fn every_message_matches_module_c() {
    let d: Vec<u8> = (0..64).map(|i: u32| (i * 7 + 1) as u8).collect();
    let leak = |n: usize| -> &'static [u8] { d[..n].to_vec().leak() };
    let mut msgs = vec![
        Msg::Keyboard(d[..8].try_into().unwrap()),
        Msg::Nkro(d[..14].try_into().unwrap()),
        Msg::Consumer(u16::from_le_bytes([d[0], d[1]])),
        Msg::System(d[0]),
        Msg::Fn(d[0]),
        Msg::Mouse(d[..5].try_into().unwrap()),
        Msg::DevCtrl(0x53),
        Msg::RfTest {
            channel: 0x13,
            power: 5,
            mode: 1,
        },
        Msg::RfTestStop,
        Msg::Manufacturer(leak(10)),
        Msg::Product(leak(46)),
        Msg::Vpid {
            vid: 0x342D,
            pid: 0xE4F6,
        },
        Msg::Raw(d[..32].try_into().unwrap()),
    ];
    let strings = DeviceStrings {
        bt_names: [
            b"Test BT1",
            b"Test BT2",
            b"Test BT3",
            b"Test BT4",
            b"Test BT5",
        ],
        dongle_manufacturer: utf32z("Test"),
        dongle_product: utf32z("Test Dongle"),
        vid: 0x342D,
        pid: 0xE4F6,
    };
    msgs.extend(selects(&strings));
    check(&msgs, include_str!("wire_expected.txt"));
}

static MANUFACTURER: [u8; 10] = utf16z("AKKO");
static PRODUCT: [u8; 34] = utf16z("Akko 2.4G VIA KB");

#[test]
fn board_device_select_matches_module_c() {
    let strings = DeviceStrings {
        bt_names: [
            b"Akko 5075-1",
            b"Akko 5075-2",
            b"Akko 5075-3",
            b"AKKO Keyboard BT4",
            b"AKKO Keyboard BT5",
        ],
        dongle_manufacturer: &MANUFACTURER,
        dongle_product: &PRODUCT,
        vid: 0x342D,
        pid: 0xE4F6,
    };
    check(&selects(&strings), include_str!("wire_board.txt"));
}
