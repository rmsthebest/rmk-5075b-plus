//! The saved settings, as one record of bytes: what QMK keeps in its
//! EEPROM (rgb_matrix's config, rgblight's, the board's confinfo with its
//! colour indexes, the recorded channels, the keyboard's flags, and the
//! sleep timeout).
//!
//! Layout, little-endian:
//!     0  magic "AKL"      3  VERSION       4  seq u32
//!     8  rgb_matrix: enable mode h s v speed flags
//!    15  rgblight: enable mode h s v speed
//!    21  confinfo: record_channel record_last_mode rgb_enable
//!    24  colours[16]
//!    40  channels[4][90] u16
//!   760  flags (FLAG_*)
//!   761  sleep timeout (SLEEP_*, the vendor's confinfo.sleep_timeout)
//!   762  more flags (FLAG2_*)
//!   763  CRC-32 of bytes 0..763
//! `seq` counts saves, so of two good records the newer wins. Older
//! versions still load: version 3 had no second flags byte (its CRC at
//! 762; read as clear), version 2 no sleep timeout either (CRC at 761;
//! read as SLEEP_DEFAULT), version 1 no flags at all (CRC at 760; clear).

use crate::board::{CHANNELS, ConfInfo, HSVS, KEYS, MODES};
use crate::color::Hsv;
use crate::engine::MAX_BRIGHTNESS;
use crate::{Board, Config, effect, rgblight};

pub const VERSION: u8 = 4;
const MAGIC: [u8; 3] = *b"AKL";
const CHANNELS_AT: usize = 40;
const FLAGS_AT: usize = CHANNELS_AT + CHANNELS * KEYS * 2;
const SLEEP_AT: usize = FLAGS_AT + 1;
const FLAGS2_AT: usize = SLEEP_AT + 1;
const CRC_AT: usize = FLAGS2_AT + 1;
pub const LEN: usize = CRC_AT + 4;
/// Version 1's CRC, where version 2 has the flags.
const V1_CRC_AT: usize = FLAGS_AT;
/// Version 2's CRC, where version 3 has the sleep timeout.
const V2_CRC_AT: usize = SLEEP_AT;
/// Version 3's CRC, where version 4 has the second flags byte.
const V3_CRC_AT: usize = FLAGS2_AT;

/// The sleep timeouts (wls.h's enum mm_sleep_timeout): an index, 0 never,
/// then 1, 3, 5, 10, 20, 30 minutes, and 7 the vendor's default, 2 minutes.
pub const SLEEP_NEVER: u8 = 0;
pub const SLEEP_DEFAULT: u8 = 7;
const SLEEP_MINUTES: [u32; 8] = [0, 1, 3, 5, 10, 20, 30, 2];

/// Sleep timeout `index` in ms (0: never); out of range is the default.
pub fn sleep_ms(index: u8) -> u32 {
    SLEEP_MINUTES
        .get(index as usize)
        .copied()
        .unwrap_or(SLEEP_MINUTES[SLEEP_DEFAULT as usize])
        * 60_000
}

/// GU_TOGG: the GUI keys are off (in Windows mode).
pub const FLAG_NO_GUI: u8 = 1 << 0;
/// HS_CT_A: Right Ctrl sends App.
pub const FLAG_CTRL_APP: u8 = 1 << 1;
/// confinfo.devs: the selected device, QMK's DEVS_* (0 USB, 1-5 BT, 6 2.4G).
pub const FLAG_DEVS_SHIFT: u8 = 2;
pub const FLAG_DEVS_MASK: u8 = 0x7 << FLAG_DEVS_SHIFT;
/// confinfo.last_btdevs: the BT device last used, 1-3 (0 in records from
/// before it was kept; read as 1, QMK's default).
pub const FLAG_LAST_BT_SHIFT: u8 = 5;
pub const FLAG_LAST_BT_MASK: u8 = 0x3 << FLAG_LAST_BT_SHIFT;
/// Key presses wait out the debounce too (clear: they count at the first
/// reading; the firmware's scan, not the vendor's).
pub const FLAG_DEFER_PRESS: u8 = 1 << 7;
/// HS_DIR: WASD and the arrows swapped (the vendor's confinfo.dir_flag).
pub const FLAG2_DIR: u8 = 1 << 0;

/// A record that passed its checks: magic, version, CRC and every field
/// in range.
#[repr(transparent)]
pub struct Record([u8; LEN]);

impl Record {
    /// `bytes` if it's a good record.
    pub fn check(bytes: &[u8; LEN]) -> Option<&Record> {
        let crc_ok = match bytes[3] {
            1 => {
                crc32(&bytes[..V1_CRC_AT])
                    == u32::from_le_bytes(bytes[V1_CRC_AT..][..4].try_into().unwrap())
            }
            2 => {
                crc32(&bytes[..V2_CRC_AT])
                    == u32::from_le_bytes(bytes[V2_CRC_AT..][..4].try_into().unwrap())
            }
            3 => {
                crc32(&bytes[..V3_CRC_AT])
                    == u32::from_le_bytes(bytes[V3_CRC_AT..][..4].try_into().unwrap())
            }
            VERSION => crc32(&bytes[..CRC_AT]) == stored_crc(bytes),
            _ => false,
        };
        let ok = bytes[..3] == MAGIC
            && crc_ok
            && (1..effect::COUNT).contains(&bytes[9])
            && bytes[12] <= MAX_BRIGHTNESS
            && (rgblight::MODE_STATIC..=rgblight::MODES).contains(&bytes[16])
            && (bytes[21] as usize) < CHANNELS
            && bytes[24..40].iter().all(|&c| (c as usize) < HSVS.len())
            && (bytes[3] < 3 || (bytes[SLEEP_AT] as usize) < SLEEP_MINUTES.len());
        // SAFETY: Record is a repr(transparent) [u8; LEN].
        ok.then(|| unsafe { &*(bytes as *const [u8; LEN] as *const Record) })
    }

    pub fn seq(&self) -> u32 {
        u32::from_le_bytes(self.0[4..8].try_into().unwrap())
    }

    pub fn config(&self) -> Config {
        let b = &self.0;
        Config {
            enable: b[8] != 0,
            mode: b[9],
            hsv: Hsv {
                h: b[10],
                s: b[11],
                v: b[12],
            },
            speed: b[13],
            flags: b[14],
        }
    }

    pub fn rgblight(&self) -> rgblight::Config {
        let b = &self.0;
        rgblight::Config {
            enable: b[15] != 0,
            mode: b[16],
            hsv: Hsv {
                h: b[17],
                s: b[18],
                v: b[19],
            },
            speed: b[20],
        }
    }

    pub fn conf(&self) -> ConfInfo {
        let b = &self.0;
        ConfInfo {
            record_channel: b[21],
            record_last_mode: b[22],
            rgb_enable: b[23] != 0,
        }
    }

    pub fn colours(&self) -> [u8; MODES.len()] {
        self.0[24..40].try_into().unwrap()
    }

    pub fn flags(&self) -> u8 {
        if self.0[3] == 1 { 0 } else { self.0[FLAGS_AT] }
    }

    /// The second flags byte, FLAG2_*.
    pub fn flags2(&self) -> u8 {
        if self.0[3] < 4 { 0 } else { self.0[FLAGS2_AT] }
    }

    /// The sleep timeout, SLEEP_* (an index for `sleep_ms`).
    pub fn sleep(&self) -> u8 {
        if self.0[3] < 3 {
            SLEEP_DEFAULT
        } else {
            self.0[SLEEP_AT]
        }
    }

    /// Recorded colour `key` of `channel`.
    pub fn channel(&self, channel: usize, key: usize) -> u16 {
        let at = CHANNELS_AT + (channel * KEYS + key) * 2;
        u16::from_le_bytes([self.0[at], self.0[at + 1]])
    }
}

/// `board`'s saved state, `flags`, `flags2` and the `sleep` timeout as
/// record `seq`, into `out`.
pub fn encode(board: &Board, flags: u8, flags2: u8, sleep: u8, seq: u32, out: &mut [u8; LEN]) {
    let m = &board.engine.config;
    let v = &board.vendor;
    let l = &v.rgblight.config;
    out[..3].copy_from_slice(&MAGIC);
    out[3] = VERSION;
    out[4..8].copy_from_slice(&seq.to_le_bytes());
    out[8..15].copy_from_slice(&[
        m.enable as u8,
        m.mode,
        m.hsv.h,
        m.hsv.s,
        m.hsv.v,
        m.speed,
        m.flags,
    ]);
    out[15..21].copy_from_slice(&[l.enable as u8, l.mode, l.hsv.h, l.hsv.s, l.hsv.v, l.speed]);
    out[21..24].copy_from_slice(&[
        v.conf.record_channel,
        v.conf.record_last_mode,
        v.conf.rgb_enable as u8,
    ]);
    out[24..40].copy_from_slice(&v.colours);
    for (i, c) in v.channels.iter().flatten().enumerate() {
        out[CHANNELS_AT + i * 2..][..2].copy_from_slice(&c.to_le_bytes());
    }
    out[FLAGS_AT] = flags;
    out[SLEEP_AT] = sleep;
    out[FLAGS2_AT] = flags2;
    let crc = crc32(&out[..CRC_AT]);
    out[CRC_AT..].copy_from_slice(&crc.to_le_bytes());
}

/// The CRC `encode` put in `buf`: a fingerprint of its seq and settings.
/// (Not `crc32` of all of `buf`, which is the same for every record.)
pub fn stored_crc(buf: &[u8; LEN]) -> u32 {
    u32::from_le_bytes(buf[CRC_AT..].try_into().unwrap())
}

/// Which of two sectors' records is newer (0 or 1), by their `seq`s
/// (wrapping; a tie is 0's), or `None` if neither has one.
pub fn newer(seqs: [Option<u32>; 2]) -> Option<u32> {
    match seqs {
        [Some(a), Some(b)] => Some(if b.wrapping_sub(a) as i32 > 0 { 1 } else { 0 }),
        [Some(_), None] => Some(0),
        [None, Some(_)] => Some(1),
        [None, None] => None,
    }
}

/// CRC-32 (IEEE, reflected), bit by bit: a record is checked once a boot.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trip() {
        static DEFAULTS: [[u16; KEYS]; CHANNELS] = [[0x1234; KEYS]; CHANNELS];
        let defaults = &DEFAULTS;
        let mut b = Board::new(None, defaults, false, 0);
        b.key(Key::RgbMod, 0, 0, true, false, 0);
        b.key(Key::RgbSai, 0, 0, true, false, 10);
        b.key(Key::RlMod, 0, 0, true, false, 20);
        b.vendor.channels[2][7] = 0xABCD;
        let mut buf = [0; LEN];
        encode(&b, FLAG_CTRL_APP, FLAG2_DIR, 4, 7, &mut buf);
        let r = Record::check(&buf).unwrap();
        assert_eq!(r.seq(), 7);
        assert_eq!(r.flags(), FLAG_CTRL_APP);
        assert_eq!(r.flags2(), FLAG2_DIR);
        assert_eq!(r.sleep(), 4);
        let c = Board::new(Some(r), defaults, false, 0);
        assert_eq!(c.engine.config, b.engine.config);
        assert_eq!(c.vendor.rgblight.config, b.vendor.rgblight.config);
        assert_eq!(c.vendor.conf, b.vendor.conf);
        assert_eq!(c.vendor.colours, b.vendor.colours);
        assert_eq!(c.vendor.channels, b.vendor.channels);
        let mut again = [0; LEN];
        encode(&c, r.flags(), r.flags2(), r.sleep(), 7, &mut again);
        assert_eq!(again, buf);

        b.key(Key::RgbVad, 0, 0, true, false, 30);
        encode(&b, FLAG_CTRL_APP, FLAG2_DIR, 4, 7, &mut again);
        assert_ne!(stored_crc(&again), stored_crc(&buf));

        buf[100] ^= 1;
        assert!(Record::check(&buf).is_none());
    }

    #[test]
    fn version_1_loads() {
        static DEFAULTS: [[u16; KEYS]; CHANNELS] = [[0; KEYS]; CHANNELS];
        let mut b = Board::new(None, &DEFAULTS, false, 0);
        b.key(Key::RgbVad, 0, 0, true, false, 0);
        let mut buf = [0xFF; LEN];
        encode(&b, FLAG_NO_GUI, 0, 1, 3, &mut buf);
        // As version 1 wrote it: no flags byte, the CRC in its place.
        buf[3] = 1;
        let crc = crc32(&buf[..V1_CRC_AT]);
        buf[V1_CRC_AT..][..4].copy_from_slice(&crc.to_le_bytes());
        buf[V1_CRC_AT + 4] = 0xFF;
        let r = Record::check(&buf).unwrap();
        assert_eq!(r.flags(), 0);
        assert_eq!(r.sleep(), SLEEP_DEFAULT);
        assert_eq!(
            Board::new(Some(r), &DEFAULTS, false, 0).engine.config,
            b.engine.config
        );
    }

    #[test]
    fn version_2_loads() {
        static DEFAULTS: [[u16; KEYS]; CHANNELS] = [[0; KEYS]; CHANNELS];
        let b = Board::new(None, &DEFAULTS, false, 0);
        let mut buf = [0xFF; LEN];
        encode(&b, FLAG_DEFER_PRESS, 0, 2, 3, &mut buf);
        // As version 2 wrote it: no sleep byte, the CRC in its place.
        buf[3] = 2;
        let crc = crc32(&buf[..V2_CRC_AT]);
        buf[V2_CRC_AT..][..4].copy_from_slice(&crc.to_le_bytes());
        buf[V2_CRC_AT + 4] = 0xFF;
        let r = Record::check(&buf).unwrap();
        assert_eq!(r.flags(), FLAG_DEFER_PRESS);
        assert_eq!(r.sleep(), SLEEP_DEFAULT);
    }

    #[test]
    fn version_3_loads() {
        static DEFAULTS: [[u16; KEYS]; CHANNELS] = [[0; KEYS]; CHANNELS];
        let b = Board::new(None, &DEFAULTS, false, 0);
        let mut buf = [0xFF; LEN];
        encode(&b, FLAG_NO_GUI, FLAG2_DIR, 5, 3, &mut buf);
        // As version 3 wrote it: no second flags byte, the CRC in its place.
        buf[3] = 3;
        let crc = crc32(&buf[..V3_CRC_AT]);
        buf[V3_CRC_AT..][..4].copy_from_slice(&crc.to_le_bytes());
        let r = Record::check(&buf).unwrap();
        assert_eq!(r.flags(), FLAG_NO_GUI);
        assert_eq!(r.sleep(), 5);
        assert_eq!(r.flags2(), 0);
    }

    #[test]
    fn sleep_timeouts() {
        assert_eq!(sleep_ms(SLEEP_NEVER), 0);
        assert_eq!(sleep_ms(1), 60_000);
        assert_eq!(sleep_ms(6), 30 * 60_000);
        assert_eq!(sleep_ms(SLEEP_DEFAULT), 2 * 60_000);
        assert_eq!(sleep_ms(200), 2 * 60_000);
    }

    #[test]
    fn newer_wraps() {
        assert_eq!(newer([Some(u32::MAX), Some(0)]), Some(1));
        assert_eq!(newer([Some(0), Some(u32::MAX)]), Some(0));
        assert_eq!(newer([Some(1), Some(2)]), Some(1));
        assert_eq!(newer([Some(5), Some(5)]), Some(0));
        assert_eq!(newer([None, Some(7)]), Some(1));
        assert_eq!(newer([Some(7), None]), Some(0));
        assert_eq!(newer([None, None]), None);
    }
}
