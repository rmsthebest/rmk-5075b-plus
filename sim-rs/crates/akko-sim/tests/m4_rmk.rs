//! ../../sim/tests/m4_rmk.robot: the RMK keyboard image (akko-rmk) on the
//! USB host, with the key matrix, knob and Win/Mac switch on the GPIOs and
//! RMK's storage on the SPI NOR flash. Every test starts from a blank flash
//! unless it says otherwise; the first boot then writes the default keymap,
//! which takes about 1.6 s before USB comes up. "In DFU" means the CPU is
//! parked in the ROM stub.

mod common;

use akko_sim::usb_host::{CONFIGURED, cmd, hex};
use akko_sim::{Image, Part};
use common::*;
use cortex_m3_emu::{Bus, Size};
use regex::Regex;

const PANIC_RECORD: u32 = 0x2000_7000;

/// Bytes `len(m) LE, m` hashed with FNV-1a, as boot-guard's panic record.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5u32, |h, &b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
}

#[test]
fn enumerates_as_the_qmk_keyboard_with_vial() {
    let mut t = T::boot();
    t.log_should_show("string 2 = 'AKKO 5075B Plus (RMK)'", 4.0);
    t.log_should_show(
        "configured: if 0 class 03 eps [81 01], if 1 class 03 eps [82], if 2 class 03 eps [83 02]",
        4.0,
    );
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.no_errors();
}

#[test]
fn a_key_press_sends_a_keyboard_report() {
    let mut t = T::boot_and_enumerate();
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    // No longer the scan's diagnostic: off, as in QMK without Win lock.
    assert!(!t.win_lock_led_on());
    t.key(3, 1, false);
    t.hid_should_show(1, "00 00 00 00 00 00 00 00", 1.0);
    t.no_errors();
}

/// QMK's ChibiOS driver, which works on the board, clears INCSR2/OUTCSR2
/// when it opens an endpoint and starts each IN packet with a plain write
/// of InPktRdy. musb left both CSR2s as they were and set InPktRdy with a
/// read-modify-write; keyboard reports on EP1 never reached the host on the
/// board (the knob on EP2 did).
#[test]
fn endpoints_are_driven_as_chibios_drives_them() {
    let mut t = T::boot();
    t.b.chip.usb.borrow_mut().trace = Some(Vec::new());
    // In the order embassy-usb opens them: keyboard EP1 IN and OUT,
    // composite EP2 IN, Vial EP3 IN and EP2 OUT.
    for w in ["i1 +0x12", "i1 +0x15", "i2 +0x12", "i3 +0x12", "i2 +0x15"] {
        t.usb_write_should_be(&format!("USBT W {w} <- 0x00"), 4.0);
    }
    t.wait_for_enumeration();
    t.key(3, 1, true);
    t.usb_write_should_be("USBT W i1 +0x11 <- 0x01", 1.0);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.hid_should_show(1, "00 00 00 00 00 00 00 00", 1.0);
    let d = t.b.chip.usb.borrow();
    let rmw = d
        .trace
        .as_ref()
        .unwrap()
        .iter()
        .find(|a| a.to_string() == "USBT W i1 +0x11 <- 0x03");
    assert!(rmw.is_none(), "{rmw:?}");
}

#[test]
fn fn_layer_sends_consumer_keys() {
    let mut t = T::boot_and_enumerate();
    t.key(5, 10, true);
    t.b.run_for(0.02);
    t.tap(0, 6);
    t.hid_matching(2, ".*cd 00", 1.0);
    t.key(5, 10, false);
}

#[test]
fn the_knob_turns_the_volume() {
    let mut t = T::boot_and_enumerate();
    t.knob_step(true, 0.002);
    t.hid_matching(2, ".*e9 00", 1.0);
    t.knob_step(false, 0.002);
    t.hid_matching(2, ".*ea 00", 1.0);
}

/// Three detents in 3 ms, faster than a step's 5 ms tap: the steps queue
/// and each one is sent.
#[test]
fn a_fast_turn_keeps_every_step() {
    let mut t = T::boot_and_enumerate();
    for _ in 0..3 {
        t.knob_step(false, 0.00025);
    }
    for _ in 0..3 {
        t.hid_should_show(2, "03 ea 00", 2.0);
        t.hid_should_show(2, "03 00 00", 2.0);
    }
}

#[test]
fn the_mac_switch_swaps_gui_and_alt() {
    let mut t = T::boot_and_enumerate();
    t.key(5, 1, true);
    t.hid_should_show(1, "08 00 00 00 00 00 00 00", 1.0);
    t.key(5, 1, false);
    t.b.run_for(0.03);
    t.mac_mode(true);
    t.key(5, 1, true);
    t.hid_should_show(1, "04 00 00 00 00 00 00 00", 1.0);
    t.key(5, 1, false);
    t.b.run_for(0.03);
    // Back to Win.
    t.mac_mode(false);
    t.key(5, 1, true);
    t.hid_should_show(1, "08 00 00 00 00 00 00 00", 1.0);
}

/// KC_CUT and friends, set over Vial (not in the default keymap): Ctrl+X in
/// Windows mode, Cmd+C in Mac mode, not their HID usages, which most hosts
/// ignore.
#[test]
fn the_edit_keys_send_ctrl_or_cmd_shortcuts() {
    let mut t = T::boot_and_enumerate();
    // dynamic_keymap_set_keycode: layer 0 (3, 1) = KC_CUT, layer 2 = KC_COPY.
    t.raw_hid_send(&[0x05, 0, 3, 1, 0x00, 0x7B]);
    t.b.run_for(0.05);
    t.raw_hid_send(&[0x05, 2, 3, 1, 0x00, 0x7C]);
    t.b.run_for(0.05);
    t.key(3, 1, true);
    t.hid_should_show(1, "01 00 1b 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.hid_should_show(1, "00 00 00 00 00 00 00 00", 1.0);
    t.mac_mode(true);
    t.key(3, 1, true);
    t.hid_should_show(1, "08 00 06 00 00 00 00 00", 1.0);
}

#[test]
fn without_the_mac_feature_hs_siri_sends_nothing() {
    let mut t = T::boot_and_enumerate();
    t.mac_mode(true);
    t.key(0, 4, true);
    t.b.run_for(0.05);
    t.hid_not_since(1, "08 00 2c");
}

/// Built with `mac`, in Mac mode: HS_SIRI is Cmd+Space, Mission Control
/// Ctrl+Up, and the Mac layer's transparent F5/F6 fall through to the
/// Windows layer's F5/F6, as on QMK.
#[test]
fn the_mac_feature_sends_the_vendors_mac_keys() {
    let mut t = T::start(Image::AkkoRmkMac);
    t.wait_for_enumeration();
    t.mac_mode(true);
    for (col, want) in [
        (4, "08 00 2c"),
        (3, "01 00 52"),
        (5, "00 00 3e"),
        (6, "00 00 3f"),
    ] {
        t.key(0, col, true);
        t.hid_should_show(1, &format!("{want} 00 00 00 00 00"), 1.0);
        t.key(0, col, false);
        t.b.run_for(0.03);
    }
}

/// As QMK's layer_move(0): Fn held in Mac mode, the switch to Windows,
/// then (0, 13) is Delete, not the Fn layer's RGB_MOD.
#[test]
fn with_the_mac_feature_the_switch_drops_a_held_fn_layer() {
    let mut t = T::start(Image::AkkoRmkMac);
    t.wait_for_enumeration();
    t.mac_mode(true);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.mac_mode(false);
    t.key(0, 13, true);
    t.hid_should_show(1, "00 00 4c 00 00 00 00 00", 1.0);
}

#[test]
fn the_caps_lock_led_follows_the_host() {
    let mut t = T::boot_and_enumerate();
    // RMK's LED reader fails its first read (before configuration) and
    // then sleeps a second before it reads again.
    t.b.run_for(1.1);
    t.b.host.command(0x0203);
    t.b.run_for(0.05);
    assert_eq!(t.b.word(GPIOC_ODR) & CAPS_LED, 0);
    t.b.host.command(0x0003);
    t.b.run_for(0.05);
    assert_eq!(t.b.word(GPIOC_ODR) & CAPS_LED, CAPS_LED);
}

#[test]
fn fn_right_shift_esc_enters_dfu() {
    let mut t = T::boot_and_enumerate();
    t.key(5, 10, true);
    t.b.run_for(0.02);
    t.key(4, 12, true);
    t.b.run_for(0.02);
    t.key(0, 0, true);
    t.b.run_for(0.03);
    t.pc_should_be_in_rom();
}

/// VIA's id_bootloader_jump (0x0B), through RMK's bootloader hook and
/// boot-guard.
#[test]
fn vials_bootloader_button_enters_dfu() {
    let mut t = T::boot_and_enumerate();
    t.raw_hid_send(&[0x0B]);
    t.b.run_for(0.05);
    t.pc_should_be_in_rom();
}

/// VIA's DynamicKeymapSetBuffer (0x13), as its "load layout" sends it: 28
/// bytes, 14 big-endian keycodes. RMK read 28 keycodes from the 32-byte
/// report (a panic) and little-endian. GetBuffer (0x12) reads them back,
/// the keys type them, and they survive a reset.
#[test]
fn via_bulk_keymap_write_sets_and_keeps_keys() {
    let mut t = T::boot_and_enumerate();
    // Layer 0 row 0, from column 1: 14 keys of KC_B (0x0005).
    let mut p = vec![0x13, 0x00, 0x02, 28];
    p.extend([0x00, 0x05].repeat(14));
    t.raw_hid_send(&p);
    t.b.run_for(0.05);
    t.no_panic_since_start_up();
    t.raw_hid_send(&[0x12, 0x00, 0x02, 28]);
    t.hid_should_show(3, &format!("12 00 02 1c{}", " 00 05".repeat(14)), 1.0);
    t.tap(0, 1);
    t.hid_should_show(1, "00 00 05 00 00 00 00 00", 1.0);
    // Saved: give storage time, then reset without erasing the flash.
    t.b.run_for(1.0);
    t.sys_reset_and_enumerate(1.0);
    t.tap(0, 14);
    t.hid_should_show(1, "00 00 05 00 00 00 00 00", 1.0);
    t.no_errors();
}

/// CLEAR_FEATURE(ENDPOINT_HALT) for endpoints the chip doesn't have (musb
/// indexed its waker arrays with the host's number and panicked), and a
/// control read abandoned after its SETUP (SetupEnd). USB keeps working
/// without a reset: no panic was recorded.
#[test]
fn odd_host_requests_dont_crash_usb() {
    let mut t = T::boot_and_enumerate();
    t.b.host.command(cmd::CLEAR_HALT | 0x85 << 8);
    t.log_should_show("CLEAR_FEATURE(ENDPOINT_HALT, 0x85) -> ok", 1.0);
    t.b.host.command(cmd::CLEAR_HALT | 0x0F << 8);
    t.log_should_show("CLEAR_FEATURE(ENDPOINT_HALT, 0x0f) -> ok", 1.0);
    // The status stage completes before the request is handled, so "ok"
    // doesn't rule out a panic after it.
    t.b.run_for(0.05);
    t.no_panic_since_start_up();
    t.b.host.command(cmd::ABORTED_SETUP);
    t.log_should_show(
        "GET_DESCRIPTOR(device, after an aborted transfer) -> 18 bytes",
        1.0,
    );
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.tap(3, 1);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.no_panic_since_start_up();
}

/// A SETUP that isn't 8 bytes: musb's setup() looped on it without acking
/// RxPktRdy, so the CPU spun until the watchdog reset. Now it is acked and
/// stalled, and the next request is answered. The host waits 2.5 s before
/// that request, past the watchdog.
#[test]
fn a_short_setup_is_dropped() {
    let mut t = T::boot_and_enumerate();
    t.b.host.command(cmd::SHORT_SETUP);
    t.log_should_show(
        "GET_DESCRIPTOR(device, after a short SETUP) -> 18 bytes",
        4.0,
    );
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.tap(3, 1);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.no_panic_since_start_up();
}

#[test]
fn esc_at_a_cold_start_enters_dfu() {
    let mut t = T::boot();
    t.key(0, 0, true);
    t.b.run_for(0.01);
    t.pc_should_be_in_rom();
}

#[test]
fn storage_stays_out_of_qmks_block() {
    let mut t = T::boot();
    // A marker in each 4 KB sector of the QMK block.
    for a in (0..0x10000).step_by(0x1000) {
        t.b.nor.borrow_mut().set_word(a, 0xA500_0000 | a as u32);
    }
    t.wait_for_enumeration();
    for a in (0..0x10000).step_by(0x1000) {
        assert_eq!(t.b.nor.borrow().word(a), 0xA500_0000 | a as u32);
    }
    let re = Regex::new(r"^(CE|(SE|BE|PP) 0x00)").unwrap();
    let hits: Vec<String> =
        t.b.log(Part::Nor)
            .into_iter()
            .filter(|l| re.is_match(l))
            .collect();
    assert!(hits.is_empty(), "{hits:?}");
    // RMK's sectors are its whole window, 0x10000-0x1FFFF.
    assert_ne!(t.b.nor.borrow().mem[0x10000], 0xFF);
    t.no_errors();
}

#[test]
fn a_keymap_change_through_vial_survives_a_reset() {
    let mut t = T::boot_and_enumerate();
    // VIA protocol version.
    t.raw_hid_send(&[0x01]);
    t.hid_should_show(3, "01 00 09", 1.0);
    // dynamic_keymap_set_keycode(layer 0, row 3, col 1) = B, then read it back.
    t.raw_hid_send(&[0x05, 0, 3, 1, 0x00, 0x05]);
    t.b.run_for(0.05);
    t.raw_hid_send(&[0x04, 0, 3, 1]);
    t.hid_should_show(3, "04 00 03 01 00 05", 1.0);
    t.b.run_for(0.5);
    t.sys_reset_and_enumerate(1.0);
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 05 00 00 00 00 00", 1.0);
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}

/// VIA id_custom_get_value on channel 0xD1 answers with the scan's
/// diagnostics (src/scan.rs fill_diag): a pass every 1 ms while a key is
/// down (on the board, a TIM2 time base made it one per 65.5 ms), none
/// while parked, one debounced event, and the held key in the readings
/// since the last query.
#[test]
fn the_scan_reports_its_counters_over_vial() {
    let mut t = T::boot_and_enumerate();
    t.raw_hid_send(&[0x08, 0xD1]);
    let first = t.hid_matching(3, "08 d1 .*", 1.0);
    t.b.run_for(0.3);
    t.raw_hid_send(&[0x08, 0xD1]);
    let idle = t.hid_matching(3, "08 d1 .*", 1.0);
    assert_eq!(first[2..6], idle[2..6], "the scan made passes while parked");
    t.key(3, 1, true);
    t.b.run_for(0.32);
    t.raw_hid_send(&[0x08, 0xD1]);
    let second = t.hid_matching(
        3,
        "08 d1( [0-9a-f]{2}){12} 01 00 00 00( [0-9a-f]{2}){2} 00 00 00 02 00 00 00 00 00 00 00 00",
        1.0,
    );
    let passes = le32(&second[2..]) - le32(&first[2..]);
    assert!(passes >= 290, "only {passes} scan passes in 0.32 s");
    let gap = le16(&second[18..]);
    assert!(
        gap <= 25,
        "longest gap between passes {} ms",
        gap as f64 / 10.0
    );
    // Other channels stay unanswered.
    t.raw_hid_send(&[0x08, 0x01]);
    t.hid_should_show(3, "08 01 00 00 00 00", 1.0);
}

/// 300 key edits through Vial, each saved to the SPI flash by RMK's
/// storage (which fills a sector and moves to the next), while the scan's
/// longest gap between passes stays short. Storage reads are polled at 3
/// MHz; uncached, it walked whole sectors per edit without letting the scan
/// run. A key is held throughout, so the scan isn't parked.
#[test]
fn vial_edits_dont_hold_up_the_scan() {
    let mut t = T::boot_and_enumerate();
    t.key(0, 5, true);
    t.b.run_for(1.0);
    t.raw_hid_send(&[0x08, 0xD1]);
    t.hid_matching(3, "08 d1 .*", 1.0);
    for i in 0..300u32 {
        let kc = 0x04 + (i % 26) as u8;
        t.raw_hid_send(&[
            0x05,
            (i % 2) as u8,
            ((i / 15) % 6) as u8,
            (i % 15) as u8,
            0x00,
            kc,
        ]);
        t.b.run_for(0.01);
    }
    t.b.run_for(1.0);
    t.raw_hid_send(&[0x08, 0xD1]);
    let reply = t.hid_matching(3, "08 d1 .*", 1.0);
    let gap = le16(&reply[18..]);
    t.key(0, 5, false);
    println!(
        "longest scan gap during Vial edits: {} ms",
        gap as f64 / 10.0
    );
    // 34.8 ms before the page caches and the read yields.
    assert!(
        gap <= 50,
        "longest gap between passes {} ms",
        gap as f64 / 10.0
    );
    t.no_panic_since_start_up();
    t.no_errors();
}

/// Eager press, deferred release (src/scan.rs): the report for a press goes
/// out within 3 ms (it took 5-6 ms with presses debounced), and a 2 ms
/// bounce while releasing doesn't release the key.
#[test]
fn a_press_counts_at_once_a_release_after_5_ms() {
    let mut t = T::boot_and_enumerate();
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 0.003);
    t.b.run_for(0.02);
    t.key(3, 1, false);
    t.b.run_for(0.002);
    t.key(3, 1, true);
    t.b.run_for(0.02);
    t.hid_not_since(1, "00 00 00 00 00 00 00 00");
    t.key(3, 1, false);
    t.hid_should_show(1, "00 00 00 00 00 00 00 00", 0.01);
}

/// Channel 0xD6: 2 makes presses wait the 5 ms too (for a chattering
/// switch), 1 goes back; the setting is saved with the lighting record and
/// survives a reset.
#[test]
fn presses_can_wait_out_the_debounce_set_over_vial() {
    let mut t = T::boot_and_enumerate();
    t.raw_hid_send(&[0x08, 0xD6, 0x00]);
    t.hid_should_show(3, "08 d6 00 00", 1.0);
    t.raw_hid_send(&[0x08, 0xD6, 0x02]);
    t.hid_should_show(3, "08 d6 02 01", 1.0);
    t.key(3, 1, true);
    t.b.run_for(0.003);
    t.hid_not_since(1, "00 00 04");
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 0.01);
    t.key(3, 1, false);
    // Saved at the next lighting frame; then a reset keeps it.
    t.b.run_for(1.0);
    t.sys_reset_and_enumerate(2.0);
    t.raw_hid_send(&[0x08, 0xD6, 0x00]);
    t.hid_should_show(3, "08 d6 00 01", 1.0);
    t.raw_hid_send(&[0x08, 0xD6, 0x01]);
    t.hid_should_show(3, "08 d6 01 00", 1.0);
}

/// Channel 0xD7 (tools/sleep.py): 0 reads the setting (7, 2 min, by
/// default), 1 + n sets it to n; out of range is ignored. It's saved with
/// the lighting record and survives a reset.
#[test]
fn the_sleep_timeout_is_set_over_vial_and_kept() {
    let mut t = T::boot_and_enumerate();
    t.raw_hid_send(&[0x08, 0xD7, 0x00]);
    t.hid_should_show(3, "08 d7 00 07", 1.0);
    t.raw_hid_send(&[0x08, 0xD7, 0x03]);
    t.hid_should_show(3, "08 d7 03 02", 1.0);
    t.raw_hid_send(&[0x08, 0xD7, 0x09]);
    t.hid_should_show(3, "08 d7 09 02", 1.0);
    t.b.run_for(1.0);
    t.sys_reset_and_enumerate(2.0);
    t.raw_hid_send(&[0x08, 0xD7, 0x00]);
    t.hid_should_show(3, "08 d7 00 02", 1.0);
    t.raw_hid_send(&[0x08, 0xD7, 0x01]);
    t.hid_should_show(3, "08 d7 01 00", 1.0);
}

/// Channel 0xD2 answers with the message boot-guard kept from the previous
/// run's panic (record at 0x20007000), 28 bytes per request.
#[test]
fn the_last_runs_panic_message_is_readable_over_vial() {
    let mut t = T::boot();
    let msg = b"firmware/src/scan.rs:1:1: a simulated panic";
    let len = (msg.len() as u32).to_le_bytes();
    let mut hashed = len.to_vec();
    hashed.extend(msg);
    let mut record = b"CINP".to_vec();
    record.extend(len);
    record.extend(fnv1a(&hashed).to_le_bytes());
    record.extend(msg);
    for (i, &b) in record.iter().enumerate() {
        t.b.chip
            .write(PANIC_RECORD + i as u32, Size::Byte, b as u32);
    }
    t.wait_for_enumeration();
    for off in (0..msg.len()).step_by(28) {
        t.raw_hid_send(&[0x08, 0xD2, off as u8]);
        let part = hex(&msg[off..(off + 28).min(msg.len())]);
        t.hid_should_show(3, &format!("08 d2 {off:02x} {:02x} {part}", msg.len()), 1.0);
    }
}

/// boot-guard's crash policy, through the deliberate panic on Vial channel
/// 0xD3 (argument 'P'). The second panic comes well within STABLE_AFTER
/// (60 s), so it goes to DFU.
#[test]
fn a_panic_resets_into_the_keyboard_a_second_one_enters_dfu() {
    let mut t = T::boot_and_enumerate();
    // Without the confirmation byte, nothing happens.
    t.raw_hid_send(&[0x08, 0xD3, 0x00]);
    t.hid_should_show(3, "08 d3 00 00", 1.0);
    t.raw_hid_send(&[0x08, 0xD3, 0x50]);
    t.b.run_for(0.01);
    assert!(t.b.in_flash(), "PC 0x{:08x}", t.b.pc());
    t.log_should_show("configured", 4.0);
    t.b.run_for(0.05);
    t.raw_hid_send(&[0x08, 0xD2, 0]);
    // The message starts "firmware/src/main.rs:".
    t.hid_matching(
        3,
        &format!(
            "08 d2 00 [0-9a-f]{{2}} {} .*",
            hex(b"firmware/src/main.rs:")
        ),
        1.0,
    );
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.raw_hid_send(&[0x08, 0xD3, 0x50]);
    t.b.run_for(0.01);
    t.pc_should_be_in_rom();
}

/// The software watchdog (src/watchdog.rs): Vial channel 0xD8 (argument
/// 'H') hangs the executor; within 2-2.25 s TIM2's check panics, with the
/// watchdog's message, and boot-guard resets. A second hang before the
/// image is stable goes to DFU.
#[test]
fn a_hang_trips_the_watchdog_a_reset_then_dfu() {
    let mut t = T::boot_and_enumerate();
    t.raw_hid_send(&[0x08, 0xD8, 0x00]);
    t.hid_should_show(3, "08 d8 00 00", 1.0);
    t.raw_hid_send(&[0x08, 0xD8, 0x48]);
    t.b.run_for(1.9);
    // Still hung: no reset yet (a reset would re-enumerate).
    assert_eq!(t.b.resets, 0);
    t.b.run_for(0.5);
    t.log_should_show("configured", 4.0);
    t.b.run_for(0.05);
    t.raw_hid_send(&[0x08, 0xD2, 0]);
    // The message starts "firmware/src/watchdog.rs:LL:" (28 bytes with a
    // two-digit line), and the second part is "CC: watchdog: ...".
    t.hid_matching(
        3,
        &format!(
            "08 d2 00 [0-9a-f]{{2}} {} .*",
            hex(b"firmware/src/watchdog.rs:")
        ),
        1.0,
    );
    t.raw_hid_send(&[0x08, 0xD2, 28]);
    t.hid_matching(
        3,
        &format!(
            "08 d2 1c [0-9a-f]{{2}} (3[0-9] )+{} .*",
            hex(b": watchdog:")
        ),
        1.0,
    );
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.raw_hid_send(&[0x08, 0xD8, 0x48]);
    t.b.run_for(2.5);
    t.pc_should_be_in_rom();
}

/// Stack high-water mark over first boot (default keymap written to flash),
/// typing, the knob and Vial; fails under 12 KB spare (16.9 KB after the
/// 2026-10-27 headroom pass; 4.9 KB before).
#[test]
fn the_stack_has_headroom() {
    let mut t = T::boot();
    // The stack is RAM below _stack_start (cortex-m-rt puts it at the
    // bottom): painted before the CPU runs.
    let top = t.stack_start();
    for a in (0x2000_0000..top).step_by(4) {
        t.b.chip.write(a, Size::Word, 0xAAAA_AAAA);
    }
    t.wait_for_enumeration();
    t.tap(3, 1);
    t.key(5, 10, true);
    t.tap(0, 6);
    t.key(5, 10, false);
    t.knob_step(true, 0.002);
    t.knob_step(false, 0.002);
    t.raw_hid_send(&[0x01]);
    t.hid_should_show(3, "01 00 09", 1.0);
    t.raw_hid_send(&[0x05, 0, 3, 1, 0x00, 0x05]);
    t.b.run_for(0.05);
    t.raw_hid_send(&[0x04, 0, 3, 1]);
    t.hid_should_show(3, "04 00 03 01 00 05", 1.0);
    t.b.run_for(0.5);
    // The lowest 16-byte step that isn't paint.
    let mut a = 0x2000_0000;
    while a < top && t.b.word(a) == 0xAAAA_AAAA {
        a += 16;
    }
    let (used, size) = (top - a, top - 0x2000_0000);
    println!("stack: {used} of {size} bytes used");
    assert!(
        size - used >= 12288,
        "only {} bytes of stack spare",
        size - used
    );
}
