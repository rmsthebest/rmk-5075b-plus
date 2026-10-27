//! Sleep (M7b).
//!
//! The sleep timeout is the vendor's setting (`record::SLEEP_*`: never, 1,
//! 3, 5, 10, 20, 30 minutes, default 2), set over Vial channel 0xD7
//! (tools/sleep.py) and saved with the lighting record.

use core::sync::atomic::{AtomicU8, Ordering};

use lighting_crate::record;

static SETTING: AtomicU8 = AtomicU8::new(record::SLEEP_DEFAULT);

/// The sleep timeout, `record::SLEEP_*`.
pub fn setting() -> u8 {
    SETTING.load(Ordering::Relaxed)
}

/// Sets the sleep timeout (from the record at start-up, or Vial); out of
/// range is ignored.
pub fn set_setting(index: u8) {
    if index <= record::SLEEP_DEFAULT {
        SETTING.store(index, Ordering::Relaxed);
    }
}
