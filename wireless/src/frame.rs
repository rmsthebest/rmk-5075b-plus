//! What the keyboard sends the module, and the bytes for it (module.c's
//! md_send_*).

/// The longest frame: a dongle string, `cmd len 46 bytes sum`.
pub const MAX_FRAME: usize = STRING_MAX + 3;
/// The module's ACK, sent back for every good frame either way.
pub const ACK: [u8; 3] = [0x61, 0x0D, 0x0A];
/// Raw HID (VIA) payload: always one 32-byte report.
pub const RAW_LEN: usize = 32;
/// A Bluetooth device name.
pub const DEVINFO_MAX: usize = 18;
/// A dongle string (UTF-16LE).
pub const STRING_MAX: usize = 46;

/// Frame commands, keyboard to module.
pub mod cmd {
    pub const KEYBOARD: u8 = 0xA1;
    pub const NKRO: u8 = 0xA2;
    pub const CONSUMER: u8 = 0xA3;
    pub const SYSTEM: u8 = 0xA4;
    pub const FN: u8 = 0xA5;
    pub const DEVCTRL: u8 = 0xA6;
    pub const MOUSE: u8 = 0xA8;
    pub const DEVINFO: u8 = 0xA9;
    pub const MANUFACTURER: u8 = 0xAB;
    pub const PRODUCT: u8 = 0xAC;
    pub const VPID: u8 = 0xAD;
    pub const RAW: u8 = 0xAF;
    pub const RAW_IN: u8 = 0x61;
    pub const RF_TEST: u8 = 0xB3;
    pub const RF_TEST_STOP: u8 = 0xB4;
}

/// `cmd::DEVCTRL` arguments.
pub mod devctrl {
    pub const USB: u8 = 0x11;
    pub const DONGLE: u8 = 0x30;
    /// BT1; BT2-BT5 follow.
    pub const BT1: u8 = 0x31;
    pub const PAIR: u8 = 0x51;
    pub const CLEAN: u8 = 0x52;
    pub const INQUIRE_BATTERY: u8 = 0x53;
    pub const SLEEP_BT_EN: u8 = 0x55;
    pub const SLEEP_BT_DIS: u8 = 0x56;
    pub const SLEEP_2G4_EN: u8 = 0x57;
    pub const SLEEP_2G4_DIS: u8 = 0x58;
    pub const CHARGING: u8 = 0x64;
    pub const CHARGING_STOP: u8 = 0x65;
    pub const CHARGING_DONE: u8 = 0x66;
    pub const FW_VERSION: u8 = 0x70;
}

/// One message for the module. Queued as a value and encoded when sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The 8-byte boot keyboard report: modifiers, reserved, 6 keys.
    Keyboard([u8; 8]),
    Nkro([u8; 14]),
    /// A consumer usage (0 = released).
    Consumer(u16),
    /// System control as the module's bitmap: bit 0 power down, 1 sleep, 2 wake.
    System(u8),
    Fn(u8),
    /// Buttons, x, y, horizontal wheel, vertical wheel.
    Mouse([u8; 5]),
    DevCtrl(u8),
    /// A Bluetooth device name, at most [`DEVINFO_MAX`] bytes.
    DevInfo(&'static [u8]),
    /// Dongle strings: UTF-16LE with the terminating NUL, at most [`STRING_MAX`] bytes.
    Manufacturer(&'static [u8]),
    Product(&'static [u8]),
    Vpid { vid: u16, pid: u16 },
    Raw([u8; RAW_LEN]),
    RfTest { channel: u8, power: u8, mode: u8 },
    RfTestStop,
}

impl Msg {
    /// Writes the frame; 0 when it can't be sent (a name or string too
    /// long), as module.c drops those.
    pub fn encode(&self, out: &mut [u8; MAX_FRAME]) -> usize {
        match *self {
            Msg::Keyboard(r) => report(out, cmd::KEYBOARD, &r),
            Msg::Nkro(r) => report(out, cmd::NKRO, &r),
            Msg::Consumer(usage) => report(out, cmd::CONSUMER, &usage.to_le_bytes()),
            Msg::System(bits) => report(out, cmd::SYSTEM, &[bits]),
            Msg::Fn(v) => report(out, cmd::FN, &[v]),
            Msg::Mouse(r) => report(out, cmd::MOUSE, &r),
            Msg::DevCtrl(c) => report(out, cmd::DEVCTRL, &[c]),
            Msg::DevInfo(name) => {
                // Always the full 21 bytes: the sum after the name, then zeros.
                if name.len() > DEVINFO_MAX {
                    return 0;
                }
                let n = name.len();
                out[..DEVINFO_MAX + 3].fill(0);
                out[0] = cmd::DEVINFO;
                out[1] = n as u8;
                out[2..2 + n].copy_from_slice(name);
                out[2 + n] = sum(&out[..2 + n]);
                DEVINFO_MAX + 3
            }
            Msg::Manufacturer(s) => string(out, cmd::MANUFACTURER, s),
            Msg::Product(s) => string(out, cmd::PRODUCT, s),
            Msg::Vpid { vid, pid } => {
                let [v0, v1] = vid.to_le_bytes();
                let [p0, p1] = pid.to_le_bytes();
                report(out, cmd::VPID, &[v0, v1, p0, p1])
            }
            Msg::Raw(data) => {
                out[0] = cmd::RAW;
                out[1] = cmd::RAW_IN;
                out[2] = RAW_LEN as u8;
                out[3..3 + RAW_LEN].copy_from_slice(&data);
                out[3 + RAW_LEN] = sum(&out[..3 + RAW_LEN]);
                RAW_LEN + 4
            }
            Msg::RfTest { channel, power, mode } => {
                // The stock firmware's checksum, not a sum.
                *out.first_chunk_mut().unwrap() = [
                    cmd::RF_TEST,
                    channel,
                    power,
                    mode,
                    cmd::RF_TEST.wrapping_add(channel).wrapping_sub(mode),
                ];
                5
            }
            Msg::RfTestStop => {
                *out.first_chunk_mut().unwrap() = [cmd::RF_TEST_STOP, 0, cmd::RF_TEST_STOP];
                3
            }
        }
    }

    pub fn is_keyboard(&self) -> bool {
        matches!(self, Msg::Keyboard(_) | Msg::Nkro(_))
    }
}

pub(crate) fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |s, &b| s.wrapping_add(b))
}

/// `cmd payload sum`
fn report(out: &mut [u8; MAX_FRAME], cmd: u8, payload: &[u8]) -> usize {
    let n = payload.len();
    out[0] = cmd;
    out[1..1 + n].copy_from_slice(payload);
    out[1 + n] = sum(&out[..1 + n]);
    n + 2
}

/// `cmd len bytes sum`
fn string(out: &mut [u8; MAX_FRAME], cmd: u8, s: &[u8]) -> usize {
    if s.len() > STRING_MAX {
        return 0;
    }
    let n = s.len();
    out[0] = cmd;
    out[1] = n as u8;
    out[2..2 + n].copy_from_slice(s);
    out[2 + n] = sum(&out[..2 + n]);
    n + 3
}

/// What the keyboard talks to; QMK's DEVS_* numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Device {
    Usb = 0,
    Bt1 = 1,
    Bt2 = 2,
    Bt3 = 3,
    Bt4 = 4,
    Bt5 = 5,
    Dongle = 6,
}

impl Device {
    pub fn from_u8(n: u8) -> Option<Device> {
        Some(match n {
            0 => Device::Usb,
            1 => Device::Bt1,
            2 => Device::Bt2,
            3 => Device::Bt3,
            4 => Device::Bt4,
            5 => Device::Bt5,
            6 => Device::Dongle,
            _ => return None,
        })
    }

    pub fn is_wireless(self) -> bool {
        self != Device::Usb
    }

    /// 0-4 for BT1-BT5.
    fn bt_index(self) -> Option<usize> {
        matches!(self, Device::Bt1 | Device::Bt2 | Device::Bt3 | Device::Bt4 | Device::Bt5)
            .then(|| self as usize - 1)
    }
}

/// How the keyboard introduces itself: the Bluetooth names, and what the
/// dongle shows the host over USB.
#[derive(Clone, Copy, Debug)]
pub struct DeviceStrings {
    pub bt_names: [&'static [u8]; 5],
    pub dongle_manufacturer: &'static [u8],
    pub dongle_product: &'static [u8],
    pub vid: u16,
    pub pid: u16,
}

/// The messages that select `dev` (md_devs_change). `reset` also clears
/// the device's pairing and starts pairing anew.
pub fn select(dev: Device, reset: bool, strings: &DeviceStrings, mut push: impl FnMut(Msg)) {
    match dev {
        Device::Usb => push(Msg::DevCtrl(devctrl::USB)),
        Device::Dongle => {
            push(Msg::DevCtrl(devctrl::DONGLE));
            if reset {
                push(Msg::Manufacturer(strings.dongle_manufacturer));
                push(Msg::Product(strings.dongle_product));
                push(Msg::Vpid { vid: strings.vid, pid: strings.pid });
                push(Msg::DevCtrl(devctrl::CLEAN));
                push(Msg::DevCtrl(devctrl::PAIR));
            }
        }
        _ => {
            let i = dev.bt_index().unwrap();
            push(Msg::DevCtrl(devctrl::BT1 + i as u8));
            if reset {
                push(Msg::DevCtrl(devctrl::CLEAN));
                push(Msg::DevInfo(strings.bt_names[i]));
                push(Msg::DevCtrl(devctrl::PAIR));
            }
        }
    }
}

/// UTF-16LE with a terminating NUL, as QMK's `L"..."` under -fshort-wchar.
/// `N` must be `2 * (s.len() + 1)`; `s` must be ASCII.
pub const fn utf16z<const N: usize>(s: &str) -> [u8; N] {
    let b = s.as_bytes();
    assert!(N == 2 * (b.len() + 1));
    let mut out = [0; N];
    let mut i = 0;
    while i < b.len() {
        assert!(b[i] < 0x80);
        out[2 * i] = b[i];
        i += 1;
    }
    out
}
