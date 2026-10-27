//! The radio module on UART3 (115200 8N1): Bluetooth 1-5 and the 2.4G
//! dongle. A port of QMK's keyboards/linker/wireless (module.c, smsg.c,
//! wireless.c) from the fixed tree (branch akko-5075b-plus, the commits
//! after 393a22ba6a), without I/O: the firmware feeds in received bytes
//! and the time, and writes out what [`Link`] hands it.
//!
//! Every frame either way is `cmd, payload, 8-bit sum` and is ACKed with
//! `61 0D 0A`. The keyboard sends one frame at a time and waits for the
//! ACK, retrying for a bounded time.
//!
//! The vendor bugs the fixed tree removed stay out:
//! 1. Key releases made while the link is down are not lost: the latest
//!    keyboard report is kept and sent once the module reconnects.
//! 2. A frame is retried 40 times, then dropped and counted; a dropped
//!    keyboard report is followed by the latest one.
//! 3. The receive parser resyncs after a lost byte (20 ms gap) and bounds
//!    the raw frame's length byte.
//! 4. Queued messages are typed values, encoded only when sent, so no
//!    buffer is shorter than a frame.
//! 5. While the link is down, the device select is resent at most every
//!    500 ms, not once per report.
//! 7. The VID/PID frame packs both as little-endian u16s.
//!
//! (6, the charge-state and battery polling rates, is the firmware's.)

#![no_std]

pub mod frame;
pub mod link;
pub mod rx;

pub use frame::{Device, DeviceStrings, Msg};
pub use link::{Link, ModuleState, Stats};
pub use rx::{Event, Parser};
