//! ../../sim/tests/m6_wireless.robot: the radio module on UART3 (src/radio.rs,
//! the wireless crate). The module logs every frame the keyboard sends
//! ("<- ..."), ACKs it, answers battery inquiries, and sends what a test
//! tells it to ("-> ..."). The frames' bytes are the fixed QMK tree's (the
//! wireless crate's tests check them against module.c); these tests check
//! the firmware around them: when frames go out, the pairing keys, where
//! the reports go, what's kept across a reset, sleep and stop mode.
//!
//! Neither sim models what stop mode does to the clocks (BOARD_FACTS.md):
//! a stop is SysTick off, SLEEPDEEP and WFI, which wakes on any interrupt.

mod common;

use akko_sim::Part;
use akko_sim::radio::NO_ACKS;
use akko_sim::usb_host::hex;
use common::*;
use wb32_sim::gpio::{A, B};

const BT1: (u8, u8) = (2, 3);
const BT2: (u8, u8) = (2, 4);
const DONGLE: (u8, u8) = (2, 6);
const USB: (u8, u8) = (2, 7);
const KEY_A: (u8, u8) = (3, 1);
// Frames, as the radio logs them.
const SEL_USB: &str = "a6 11 b7";
const SEL_DONGLE: &str = "a6 30 d6";
const SEL_BT2: &str = "a6 32 d8";
const ALL_UP: &str = "a1 00 00 00 00 00 00 00 00 a1";
const A_DOWN: &str = "a1 00 00 04 00 00 00 00 00 a5";
const VERSION_ASK: &str = "a6 70 16";
// Module frames: (cmd, arg).
const CONNECTED: (u8, u8) = (0x5B, 0x32);
const DISCONNECTED: (u8, u8) = (0x5B, 0x33);
const CAPS_ON: (u8, u8) = (0x5A, 0x02);
const HOST_SUSPEND: (u8, u8) = (0x60, 0x00);
const HOST_RESUME: (u8, u8) = (0x60, 0x01);
const LED_2G4: usize = 38;
const RED: u32 = 0xFF0000;
const GREEN: u32 = 0x00FF00;

/// From a blank flash, on until USB is configured (past the radio's
/// start-up frames, which go out 100 ms in).
fn boot() -> T {
    let mut t = T::boot();
    t.log_should_show("configured", 4.0);
    t.b.run_for(0.05);
    t
}

fn key(t: &mut T, (row, col): (u8, u8), down: bool) {
    t.key(row, col, down);
}

fn tap(t: &mut T, (row, col): (u8, u8)) {
    t.tap(row, col);
}

fn radio_should_get(t: &mut T, frame: &str, secs: f64) {
    t.part_should_show(Part::Radio, &format!("^<- {}", regex::escape(frame)), secs);
}

fn radio_should_not_get(t: &T, frame: &str) {
    t.part_not_since(Part::Radio, &format!("^<- {}", regex::escape(frame)));
}

fn module_sends(t: &mut T, (cmd, arg): (u8, u8)) {
    t.b.module_frame(cmd, arg);
}

/// A raw HID packet from the host through the dongle (AF 60 32 data[32]
/// sum): `bytes`, then zeros. `sum_fix` is added to the sum (non-zero: a
/// bad frame).
fn module_sends_raw(t: &mut T, bytes: &[u8], sum_fix: u8) {
    let mut frame = vec![0xAF, 0x60, 32];
    frame.extend(bytes);
    frame.resize(35, 0);
    let sum = frame.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    frame.push(sum.wrapping_add(sum_fix));
    for b in frame {
        t.b.module_sends(&[b]);
    }
}

fn no_radio_errors(t: &T) {
    assert_eq!(t.b.radio.borrow().errors, Vec::<String>::new());
}

/// B1 low: the pull-up is on.
fn usb_pullup(t: &mut T) -> bool {
    !t.pin_high(GPIOB_ODR, USB_POWER)
}

/// Caps Lock's LED (C5, active low).
fn caps_led(t: &mut T) -> bool {
    !t.pin_high(GPIOC_ODR, CAPS_LED)
}

/// Fn+Y, then the module reports the link up.
fn on_the_dongle(t: &mut T) {
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(t, SEL_DONGLE, 1.0);
    module_sends(t, CONNECTED);
    radio_should_get(t, ALL_UP, 1.0);
}

/// Asleep (src/power.rs): the LED supply and boost off, the Caps LED off,
/// and (unless `lower`) the columns driven low: C0-C3 are outputs (MODER
/// 01). Awake and idle the scan is parked, with the columns driven too, so
/// they're only checked asleep.
fn should_be_asleep(t: &mut T, asleep: bool, lower: bool) {
    let a = t.b.word(GPIOA_ODR);
    assert_eq!(a & LED_BOOST == 0, asleep, "boost");
    if asleep {
        assert_eq!(a & LED_POWER, 0, "LED power");
        assert!(!caps_led(t), "Caps LED");
        let driven = t.b.word(GPIOC_MODER) & 0xFF == 0x55;
        assert_eq!(driven, !lower, "columns driven");
    }
}

/// Over Vial channel 0xD7: 0 never, 1 = 1 min, ..., 7 = 2 min.
fn set_sleep_timeout(t: &mut T, index: u8) {
    t.raw_hid_send_and_run(&[0x08, 0xD7, index + 1]);
    t.hid_should_show(3, &format!("08 d7 {:02x} {index:02x}", index + 1), 1.0);
}

/// LED `i`'s colours, every 50 ms for `secs`.
fn led_colours_over(t: &mut T, i: usize, secs: f64) -> Vec<u32> {
    (0..(secs / 0.05).round() as usize)
        .map(|_| {
            t.b.run_for(0.05);
            t.led(i)
        })
        .collect()
}

/// Vial channel 0xD9: 0x53 ('S') on, 1 off, 0 reads.
fn stop_mode(t: &mut T, arg: u8) {
    t.raw_hid_send_and_run(&[0x08, 0xD9, arg]);
}

/// In stop mode's WFI (src/stop.rs, wb32-hal stop.rs): SysTick off and
/// SLEEPDEEP set.
fn should_be_stopped(t: &mut T, stopped: bool) {
    assert_eq!(t.b.word(SYST_CSR) & 1 == 0, stopped, "SysTick off");
    assert_eq!(t.b.word(SCB_SCR) & 4 != 0, stopped, "SLEEPDEEP");
}

/// 0xD9's answer: on, the stops, those a key, the knob or the cable ended.
fn stop_counts_should_be(t: &mut T, stops: u32, wakes: u32) {
    stop_mode(t, 0);
    let mut want = vec![0x08, 0xD9, 0x00, 0x01];
    want.extend(stops.to_le_bytes());
    want.extend(wakes.to_le_bytes());
    t.hid_should_show(3, &hex(&want), 1.0);
}

/// Stop mode on, on the dongle; the host suspends: asleep at once, stopped
/// a second later (STOP_AFTER_MS).
fn asleep_and_stopped() -> T {
    let mut t = boot();
    t.b.chip.writes = Some(Vec::new());
    stop_mode(&mut t, 0x53);
    t.hid_should_show(3, "08 d9 53 01", 1.0);
    on_the_dongle(&mut t);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
    t.b.run_for(1.2);
    should_be_stopped(&mut t, true);
    // The vendor's PWR mode, STOP LP4 S32KON (from reset: CR0 0).
    let writes: Vec<String> = t.b.chip.writes.take().unwrap().iter().map(|w| w.to_string()).collect();
    for want in ["PWR+0x000 = 0x0003b004", "PWR+0x020 = 0x000003b3"] {
        assert!(writes.iter().any(|w| w == want), "no {want}");
    }
    t
}

/// One detent, A (B7) leading: volume up.
fn knob_up(t: &mut T, edge: f64) {
    for (pin, level) in [(7, Some(false)), (6, Some(false)), (7, None), (6, None)] {
        t.b.drive((B, pin), level);
        t.b.run_for(edge);
    }
}

/// 100 ms in (wireless_post_task): the module's version, its 30 min sleep
/// in BT and 2.4G mode, then the saved device (USB on a blank flash). The
/// charge state (no cable: stopped) and, every 3 s, a battery inquiry,
/// which the module answers. UART3 is set up as QMK's (the radio checks the
/// divider and line settings).
#[test]
fn start_up_talks_to_the_module_as_qmk_does() {
    let mut t = T::boot();
    // RMK opens its storage first.
    radio_should_get(&mut t, "a6 70 16", 5.0);
    radio_should_get(&mut t, "a6 55 fb", 1.0);
    radio_should_get(&mut t, "a6 57 fd", 1.0);
    radio_should_get(&mut t, SEL_USB, 1.0);
    radio_should_get(&mut t, "a6 65 0b", 1.0);
    radio_should_get(&mut t, "a6 53 f9", 4.0);
    t.part_should_show(Part::Radio, "^-> 5c 64 c0", 0.1);
    assert!(usb_pullup(&mut t));
    no_radio_errors(&t);
    t.no_errors();
}

/// Fn+Y selects 2.4G and cuts USB's pull-up; nothing is typed until the
/// module says it's connected, then the keyboard's current state goes
/// first, and keys go to the module, not USB.
#[test]
fn fn_y_moves_the_keys_to_the_dongle() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    assert!(!usb_pullup(&mut t));
    key(&mut t, KEY_A, true);
    t.b.run_for(0.05);
    radio_should_not_get(&t, "a1");
    module_sends(&mut t, CONNECTED);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.hid_not_since(1, "00 00 04");
    no_radio_errors(&t);
    t.no_errors();
}

/// Fixed-tree bug 1: the module drops out with A held, A is released, the
/// link comes back: the host gets the release. The device is re-selected
/// while down, at most every 500 ms.
#[test]
fn a_release_while_the_link_is_down_reaches_the_host_on_reconnect() {
    let mut t = boot();
    on_the_dongle(&mut t);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, A_DOWN, 1.0);
    module_sends(&mut t, DISCONNECTED);
    t.b.run_for(0.05);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    module_sends(&mut t, CONNECTED);
    radio_should_get(&mut t, ALL_UP, 1.0);
}

/// The dongle's strings, VID/PID, clear, pair: the bytes of
/// crates/qmk-wireless/tests/wire_board.txt. A shorter hold doesn't pair.
#[test]
fn fn_y_held_3_s_pairs_the_dongle_anew() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(DONGLE, 2.5);
    radio_should_not_get(&t, "a6 51 f7");
    t.fn_hold(DONGLE, 3.1);
    radio_should_get(&mut t, "ab 0a 41 00 4b 00 4b 00 4f 00 00 00 db", 1.0);
    radio_should_get(
        &mut t,
        "ac 22 41 00 6b 00 6b 00 6f 00 20 00 32 00 2e 00 34 00 47 00 20 00 56 00 49 00 41 00 20 00 4b 00 42 00 00 00 fc",
        1.0,
    );
    radio_should_get(&mut t, "ad 2d 34 f6 e4 e8", 1.0);
    radio_should_get(&mut t, "a6 52 f8", 1.0);
    radio_should_get(&mut t, "a6 51 f7", 1.0);
    no_radio_errors(&t);
}

/// BT1, clear, the name "Akko 5075-1" (DEVINFO, padded to 21 bytes), pair:
/// the bytes of crates/qmk-wireless/tests/wire_board.txt.
#[test]
fn fn_e_held_3_s_pairs_bt1_with_its_name() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(BT1, 3.1);
    radio_should_get(&mut t, "a6 31 d7", 1.0);
    radio_should_get(&mut t, "a6 52 f8", 1.0);
    radio_should_get(&mut t, "a9 0b 41 6b 6b 6f 20 35 30 37 35 2d 31 89 00 00 00 00 00 00 00", 1.0);
    radio_should_get(&mut t, "a6 51 f7", 1.0);
    no_radio_errors(&t);
}

/// The module gets an all-up report, then the USB select; the pull-up is
/// back and keys reach USB.
#[test]
fn fn_u_goes_back_to_usb() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.fn_hold(USB, 0.03);
    radio_should_get(&mut t, SEL_USB, 1.0);
    assert!(usb_pullup(&mut t));
    tap(&mut t, KEY_A);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
}

#[test]
fn the_hosts_caps_lock_comes_from_the_module() {
    let mut t = boot();
    on_the_dongle(&mut t);
    module_sends(&mut t, CAPS_ON);
    t.b.run_for(0.1);
    assert!(caps_led(&mut t));
    module_sends(&mut t, DISCONNECTED);
    t.b.run_for(0.1);
    assert!(!caps_led(&mut t));
}

/// md_raw.c: a Vial request from the module's raw frame (AF 60) is
/// answered as a raw frame (AF 61), not on USB, one after the other; a
/// frame with a bad sum is dropped.
#[test]
fn vial_works_through_the_dongle() {
    let mut t = boot();
    on_the_dongle(&mut t);
    // Sleep timeout (0xD7) to index 4: the answer is the setting.
    module_sends_raw(&mut t, &[0x08, 0xD7, 5], 0);
    radio_should_get(&mut t, "af 61 20 08 d7 05 04 00", 1.0);
    module_sends_raw(&mut t, &[0x08, 0xD7, 0], 0);
    radio_should_get(&mut t, "af 61 20 08 d7 00 04 00", 1.0);
    module_sends_raw(&mut t, &[0x08, 0xD7, 2], 1);
    t.b.run_for(0.2);
    radio_should_not_get(&t, "af 61 20 08 d7 02");
    module_sends_raw(&mut t, &[0x08, 0xD7, 0], 0);
    radio_should_get(&mut t, "af 61 20 08 d7 00 04 00", 1.0);
    t.hid_not_since(3, "08 d7");
    no_radio_errors(&t);
}

/// Fn+R (BT2), saved with the lighting record; after a reset start-up
/// selects BT2 and keeps USB's pull-up off.
#[test]
fn the_selected_device_survives_a_reset() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(BT2, 0.03);
    radio_should_get(&mut t, SEL_BT2, 1.0);
    t.part_should_show(Part::Nor, "^PP 0x02", 4.0);
    t.b.run_for(0.2);
    t.b.system_reset();
    radio_should_get(&mut t, VERSION_ASK, 6.0);
    radio_should_get(&mut t, SEL_BT2, 1.0);
    t.b.run_for(0.1);
    assert!(!usb_pullup(&mut t));
}

/// As the vendor's WLS_KEYCODE_WAKEUP: Fn+Y on the dongle selects it again,
/// so the module reconnects (a kick for a link that misbehaves).
#[test]
fn pressing_the_current_devices_key_again_selects_it_again() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.run_for(0.5);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
}

/// As the vendor's hs_reset_settings (confinfo.devs kept): Fn+` held 3 s on
/// the dongle resets the settings, and after RMK's reboot the keyboard is
/// back on the dongle, not on USB.
#[test]
fn ee_clr_keeps_the_selected_device() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.part_should_show(Part::Nor, "^PP 0x02", 4.0);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(1, 0, true);
    t.b.run_for(3.05);
    t.key(1, 0, false);
    t.b.run_for(0.03);
    t.key(5, 10, false);
    radio_should_get(&mut t, VERSION_ASK, 6.0);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.1);
    assert!(!usb_pullup(&mut t));
}

/// Fixed-tree bug 2: with the module silent, a frame is sent 41 times (10
/// ms apart), dropped, and counted; Vial channel 0xD4 reads the counters
/// (tx, ACK timeouts, retry drops, ...).
#[test]
fn an_unanswered_frame_is_dropped_after_41_tries() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.b.radio.borrow_mut().flags = NO_ACKS | 100 << 8;
    let before = t.b.radio.borrow().frames;
    t.fn_hold(BT1, 0.03);
    t.b.run_for(0.6);
    let after = t.b.radio.borrow().frames;
    assert!(after - before >= 41, "{} frames", after - before);
    t.raw_hid_send_and_run(&[0x08, 0xD4]);
    let b = t.hid_matching(3, "08 d4 .*", 1.0);
    let timeouts = u16::from_le_bytes([b[4], b[5]]);
    let drops = u16::from_le_bytes([b[6], b[7]]);
    assert!(drops >= 1, "no retry drop counted");
    assert!(timeouts >= 41, "only {timeouts} ACK timeouts");
}

/// The connection indicator (LEDs 37-41) follows the radio: after Fn+Y, Y's
/// LED blinks (500 ms, red: the vendor's PAIR colour in QMK's g r b order)
/// for as long as the link is down; CONNECTED shows it green
/// (HS_PAIR_COLOR_2G4) for 2 s, then the effect is back.
#[test]
fn the_dongles_key_blinks_until_connected_then_shows_green_for_2_s() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    let seen = led_colours_over(&mut t, LED_2G4, 2.5);
    assert!(seen.contains(&RED) && seen.contains(&0) && !seen.contains(&GREEN), "{seen:06x?}");
    module_sends(&mut t, CONNECTED);
    t.b.run_for(0.1);
    assert_eq!(t.led(LED_2G4), GREEN);
    let seen = led_colours_over(&mut t, LED_2G4, 1.6);
    assert!(!seen.contains(&RED), "{seen:06x?}");
    t.b.run_for(0.5);
    let seen = led_colours_over(&mut t, LED_2G4, 0.5);
    assert!(!seen.contains(&GREEN) && !seen.contains(&RED), "{seen:06x?}");
}

/// Sleep timeout 1 min (src/power.rs): a minute without input on the
/// dongle, it sleeps: the lights' supply, the boost and the Caps LED off,
/// the columns driven low. A key wakes it; the key reaches the host, the
/// device is selected again, the lights come back.
#[test]
fn idle_on_the_dongle_it_sleeps_and_a_key_wakes_it_and_is_typed() {
    let mut t = boot();
    on_the_dongle(&mut t);
    module_sends(&mut t, CAPS_ON);
    set_sleep_timeout(&mut t, 1);
    t.b.run_for(58.0);
    should_be_asleep(&mut t, false, false);
    assert!(caps_led(&mut t));
    t.b.run_for(3.0);
    should_be_asleep(&mut t, true, false);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, A_DOWN, 1.0);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    assert!(caps_led(&mut t));
    assert!(t.led_power(), "the lights are back");
    no_radio_errors(&t);
    t.no_errors();
}

/// With the setting "never" the keyboard stays awake with the link down;
/// with the default (2 min) it sleeps 30 s after the link went down,
/// telling the module first (devctrl USB, as the vendor).
#[test]
fn never_means_never_otherwise_a_link_down_for_30_s_sleeps() {
    let mut t = boot();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 0);
    module_sends(&mut t, DISCONNECTED);
    t.b.run_for(35.0);
    should_be_asleep(&mut t, false, false);
    set_sleep_timeout(&mut t, 7);
    t.b.run_for(0.5);
    // Input (the Vial request isn't) restarted nothing: 35 s down already.
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
}

/// As the vendor's: with the link down, keys don't restart the 30 s (only
/// a device key would), so it sleeps 30 s after the link went down. The
/// host's resume frame wakes it from any sleep.
#[test]
fn typing_doesnt_hold_off_the_link_down_sleep_the_host_resuming_wakes_it() {
    let mut t = boot();
    on_the_dongle(&mut t);
    module_sends(&mut t, DISCONNECTED);
    t.b.run_for(10.0);
    tap(&mut t, KEY_A);
    t.b.run_for(10.0);
    tap(&mut t, KEY_A);
    t.b.run_for(10.5);
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    module_sends(&mut t, HOST_RESUME);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
}

/// The module's host state frame (0x60): 0 the host suspended, 1 it
/// resumed. Also with the setting "never". A long sleep doesn't trip the
/// watchdog.
#[test]
fn the_host_suspending_sleeps_it_at_once_its_resume_wakes_it() {
    let mut t = boot();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 0);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    // Asleep, the scan runs at 10 ms: the watchdog (2 s) stays quiet. A
    // reset would have woken it.
    t.b.run_for(10.0);
    should_be_asleep(&mut t, true, false);
    assert_eq!(t.b.resets, 0);
    module_sends(&mut t, HOST_RESUME);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
}

#[test]
fn the_knob_wakes_it_and_the_turn_is_sent() {
    let mut t = boot();
    on_the_dongle(&mut t);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    knob_up(&mut t, 0.002);
    // A consumer report through the module: a3, volume up (e9).
    radio_should_get(&mut t, "a3 e9 00", 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
}

/// At 0% for a minute (M7a's timer) the module is told and the keyboard
/// sleeps; keys don't wake it (the columns stay released); the cable going
/// in (A7 high) does.
#[test]
fn a_flat_battery_sleeps_it_until_the_cable_goes_in() {
    let mut t = boot();
    t.b.radio.borrow_mut().flags = 0;
    on_the_dongle(&mut t);
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, true);
    key(&mut t, KEY_A, true);
    t.b.run_for(0.3);
    key(&mut t, KEY_A, false);
    should_be_asleep(&mut t, true, true);
    t.b.drive((A, 7), Some(true));
    t.b.run_for(0.5);
    should_be_asleep(&mut t, false, false);
    t.b.drive((A, 7), None);
}

/// Lower sleep stopped: only the cable and the UART are armed. A key
/// doesn't wake it; a frame from the module does, but it's read and the
/// keyboard stops again (the host resuming doesn't end a flat battery's
/// sleep); the cable going in ends it.
#[test]
fn in_stop_mode_a_flat_battery_stops_it_until_the_cable_goes_in() {
    let mut t = boot();
    stop_mode(&mut t, 0x53);
    t.hid_should_show(3, "08 d9 53 01", 1.0);
    t.b.radio.borrow_mut().flags = 0;
    on_the_dongle(&mut t);
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(1.5);
    should_be_asleep(&mut t, true, true);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, false);
    module_sends(&mut t, HOST_RESUME);
    t.b.run_for(1.5);
    should_be_asleep(&mut t, true, true);
    should_be_stopped(&mut t, true);
    t.b.drive((A, 7), Some(true));
    t.b.run_for(0.5);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    t.b.drive((A, 7), None);
    stop_counts_should_be(&mut t, 2, 1);
}

/// M7c's stop mode is off at reset: asleep, the clocks run.
#[test]
fn stop_mode_is_off_unless_turned_on() {
    let mut t = boot();
    on_the_dongle(&mut t);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(2.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
}

/// Stopped (SysTick off, SLEEPDEEP), the rows' EXTI lines wake it: the key
/// is read and sent, the device selected again. Ten seconds stopped don't
/// trip the watchdog (no reset: the module isn't asked its version again).
#[test]
fn in_stop_mode_a_key_wakes_it_and_is_typed() {
    let mut t = asleep_and_stopped();
    t.b.run_for(10.0);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    // Out of the stop the device is selected again before the scan reads.
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    radio_should_not_get(&t, VERSION_ASK);
    stop_counts_should_be(&mut t, 1, 1);
    no_radio_errors(&t);
    // PRE_LP, POST_LP and the clocks' set-up again open ANCTL first.
    t.no_errors();
}

/// 0xD9 'D': stop mode with the LEDs marking how far a stop got
/// (src/stop.rs). Stopped: Caps on (C5 low), Win-lock off (B0 high); a key
/// still wakes it, and the device is selected again.
#[test]
fn with_stop_diag_on_caps_shows_a_stop_in_progress() {
    let mut t = boot();
    stop_mode(&mut t, 0x44);
    t.hid_should_show(3, "08 d9 44 01", 1.0);
    on_the_dongle(&mut t);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(1.7);
    should_be_stopped(&mut t, true);
    assert!(caps_led(&mut t), "Caps on");
    assert!(t.pin_high(GPIOB_ODR, WIN_LOCK_LED), "Win-lock off");
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    key(&mut t, KEY_A, false);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
}

/// 0xD9 'D' 63: both edges, pending cleared, no knob, EXTI events, the long
/// settle, the PLL restarted (src/stop.rs VARIANT_*). Byte 16 of the answer
/// echoes the bits.
#[test]
fn every_stop_diag_variant_at_once_still_stops_and_wakes_on_a_key() {
    let mut t = boot();
    t.raw_hid_send_and_run(&[0x08, 0xD9, 0x44, 63]);
    t.hid_should_show(3, "08 d9 44 01", 1.0);
    on_the_dongle(&mut t);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(1.7);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    t.raw_hid_send_and_run(&[0x08, 0xD9, 0x00]);
    t.hid_should_show(3, "08 d9 00 01 01 00 00 00 01 00 00 00 00 00 00 00 3f", 1.0);
}

/// A frame from the module ends the stop (EXTI 11 on the chip; the UART's
/// interrupt in the sim); the radio reads it: the host resuming wakes the
/// keyboard.
#[test]
fn in_stop_mode_the_modules_uart_wakes_it() {
    let mut t = asleep_and_stopped();
    module_sends(&mut t, HOST_RESUME);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    stop_counts_should_be(&mut t, 1, 0);
}

#[test]
fn in_stop_mode_the_cable_wakes_it_and_it_doesnt_stop_with_the_cable_in() {
    let mut t = asleep_and_stopped();
    t.b.drive((A, 7), Some(true));
    t.b.run_for(0.5);
    should_be_asleep(&mut t, false, false);
    module_sends(&mut t, HOST_SUSPEND);
    t.b.run_for(2.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
    t.b.drive((A, 7), None);
}

/// B6's EXTI line wakes it. Line 7 is the cable's (A7) while stopped, so
/// the first detent's B7 edge is missed and that turn isn't read; line 7 is
/// the knob's again after, and the next turn is sent.
#[test]
fn in_stop_mode_the_knob_wakes_it_and_the_next_turn_is_sent() {
    let mut t = asleep_and_stopped();
    knob_up(&mut t, 0.002);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    should_be_stopped(&mut t, false);
    knob_up(&mut t, 0.002);
    radio_should_get(&mut t, "a3 e9 00", 1.0);
}

/// 40 detents at 4 ms each, with the module taking 4 ms to ACK each frame:
/// every step reaches the module as a volume-up press and its release. The
/// link holds 16 messages and drops what doesn't fit, so the knob waits
/// for room (src/knob.rs) instead of handing it steps the link would drop.
#[test]
fn a_fast_knob_spin_through_the_dongle_keeps_every_step() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.radio.borrow_mut().flags = 4 << 16 | 100 << 8;
    for _ in 0..40 {
        knob_up(&mut t, 0.001);
    }
    t.b.run_for(2.0);
    let presses = t.b.log(Part::Radio)[t.radio..].iter().filter(|l| l.starts_with("<- a3 e9 00")).count();
    println!("knob steps through the dongle: {presses} of 40");
    assert_eq!(presses, 40);
    no_radio_errors(&t);
}
