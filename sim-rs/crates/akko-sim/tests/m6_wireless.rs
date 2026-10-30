//! Renode's sim/tests/m6_wireless.robot: the radio module on UART3 (src/radio.rs,
//! the wireless crate). The module logs every frame the keyboard sends
//! ("<- ..."), ACKs it, answers battery inquiries, and sends what a test
//! tells it to ("-> ..."). The frames' bytes are the fixed QMK tree's (the
//! wireless crate's tests check them against module.c); these tests check
//! the firmware around them: when frames go out, the pairing keys, where
//! the reports go, what's kept across a reset, sleep and stop mode.
//!
//! A stop is the chip's (BOARD_FACTS.md): every clock off, only EXTI lines
//! wake it, and it comes back on MHSI with RCC at its reset values, so the
//! firmware must set the clocks up again; a frame that starts while UART3
//! has no clock is lost.

mod common;

use akko_sim::Part;
use akko_sim::radio::{DEVS_2G4, Fault};
use akko_sim::usb_host::hex;
use common::*;
use cortex_m3_emu::TICK_HZ;

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
const CAPS_ON: (u8, u8) = (0x5A, 0x02);
const HOST_RESUME: (u8, u8) = (0x60, 0x01);
const LED_2G4: usize = 38;
const RED: u32 = 0xFF0000;
const GREEN: u32 = 0x00FF00;
/// Per emulated second over 0.3 s, lights on, a device key tapped in each:
/// about 16 M with the module ACKing, 18 M without; 59 M while the radio
/// task spun (D-2).
const MAX_INSNS_BUSY_LINK: f64 = 30e6;

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

/// A Vial request through the dongle (USB is detached in wireless mode):
/// the answer should start with `want`'s bytes.
fn vial_via_dongle(t: &mut T, bytes: &[u8], want: &str) {
    let got = t.b.vial_via_dongle(bytes, 1.0).expect("no answer");
    assert!(hex(&got).starts_with(want), "{} for {want}", hex(&got));
    t.radio = t.b.log(Part::Radio).len();
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

/// The module has a bond with the dongle: a select connects.
fn bond_dongle(t: &mut T) {
    t.b.radio.borrow_mut().bonded[DEVS_2G4] = true;
}

/// Fn+Y with the dongle bonded: the module connects.
fn on_the_dongle(t: &mut T) {
    bond_dongle(t);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(t, SEL_DONGLE, 1.0);
    t.part_should_show(Part::Radio, "^-> 5b 32", 1.0);
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

/// Over Vial channel 0xD7, through the dongle: 0 never, 1 = 1 min, ...,
/// 7 = 2 min.
fn set_sleep_timeout(t: &mut T, index: u8) {
    vial_via_dongle(
        t,
        &[0x08, 0xD7, index + 1],
        &format!("08 d7 {:02x} {index:02x}", index + 1),
    );
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

/// ANCTL+0x004, which the vendor's stop entry and exit trim.
const ANCTL_TRIM: u32 = 0x4001_0404;

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

/// 0xD9's answer, through the dongle: the mode (1 whenever asleep), the
/// stops, those a key, the knob or the cable ended.
fn stop_counts_should_be(t: &mut T, stops: u32, wakes: u32) {
    stop_counts_in_mode(t, STOP_ALWAYS, stops, wakes);
}

/// 0xD9's modes (its answer's byte 3).
const STOP_ALWAYS: u8 = 1;
const STOP_LINK: u8 = 2;

fn stop_counts_in_mode(t: &mut T, mode: u8, stops: u32, wakes: u32) {
    let mut want = vec![0x08, 0xD9, 0x00, mode];
    want.extend(stops.to_le_bytes());
    want.extend(wakes.to_le_bytes());
    vial_via_dongle(t, &[0x08, 0xD9, 0x00], &hex(&want));
}

/// Stop mode on, on the dongle; the host suspends: asleep at once, stopped
/// a second later (STOP_AFTER_MS).
fn asleep_and_stopped() -> T {
    let mut t = boot();
    t.b.chip.writes = Some(Vec::new());
    stop_mode(&mut t, 0x53);
    t.hid_should_show(3, "08 d9 53 01", 1.0);
    on_the_dongle(&mut t);
    // On the battery: with the cable in it doesn't stop.
    t.b.plug(false);
    t.b.dongle_host(false);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
    t.b.run_for(1.2);
    should_be_stopped(&mut t, true);
    // The vendor's PWR mode, STOP LP4 S32KON (from reset: CR0 0).
    let writes: Vec<String> =
        t.b.chip
            .writes
            .take()
            .unwrap()
            .iter()
            .map(|w| w.to_string())
            .collect();
    for want in ["PWR+0x000 = 0x0003b004", "PWR+0x020 = 0x000003b3"] {
        assert!(writes.iter().any(|w| w == want), "no {want}");
    }
    // PRE_LP: ANCTL+0x004 down from the board's trim (10) to 8.
    assert_eq!(t.b.word(ANCTL_TRIM), 8);
    t
}

/// One detent clockwise, A (B7) leading: volume up.
fn knob_up(t: &mut T, edge: f64) {
    t.knob_step(true, edge);
}

/// 100 ms in (wireless_post_task): the module's version, its 30 min sleep
/// in BT and 2.4G mode, then the saved device (USB on a blank flash). The
/// charge state (the cable in, the battery full: done) and, every 3 s, a
/// battery inquiry,
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
    radio_should_get(&mut t, "a6 66 0c", 1.0);
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
    bond_dongle(&mut t);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    assert!(!usb_pullup(&mut t));
    key(&mut t, KEY_A, true);
    t.b.run_for(0.05);
    radio_should_not_get(&t, "a1");
    t.part_should_show(Part::Radio, "^-> 5b 32", 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.hid_not_since(1, "00 00 04");
    no_radio_errors(&t);
    t.no_errors();
}

/// Fixed-tree bug 1: the module drops out with A held, A is released, the
/// link comes back: the host gets the release. Keys while it's down select
/// the device again, at most every 500 ms.
#[test]
fn a_release_while_the_link_is_down_reaches_the_host_on_reconnect() {
    let mut t = boot();
    on_the_dongle(&mut t);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, A_DOWN, 1.0);
    t.b.link_drop(Some(1.5));
    t.b.run_for(0.05);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    for _ in 0..8 {
        t.b.run_for(0.1);
        tap(&mut t, (3, 2));
    }
    radio_should_get(&mut t, ALL_UP, 2.0);
    let selects = t.b.radio.borrow().selects_while_down.clone();
    assert!(selects.len() >= 2, "{} selects while down", selects.len());
    for w in selects.windows(2) {
        let gap = (w[1] - w[0]) as f64 / TICK_HZ as f64;
        assert!(gap >= 0.499, "selects {gap} s apart");
    }
    no_radio_errors(&t);
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
    radio_should_get(
        &mut t,
        "a9 0b 41 6b 6b 6f 20 35 30 37 35 2d 31 89 00 00 00 00 00 00 00",
        1.0,
    );
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
    // The host saw the pull-up go (a bus reset) and enumerates it again.
    t.log_should_show("configured", 2.0);
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
    t.b.link_drop(None);
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
    vial_via_dongle(&mut t, &[0x08, 0xD7, 5], "08 d7 05 04 00");
    vial_via_dongle(&mut t, &[0x08, 0xD7, 0], "08 d7 00 04 00");
    t.b.radio.borrow_mut().faults.push_back(Fault::BadSum);
    assert_eq!(t.b.vial_via_dongle(&[0x08, 0xD7, 2], 0.2), None);
    radio_should_not_get(&t, "af 61 20 08 d7 02");
    vial_via_dongle(&mut t, &[0x08, 0xD7, 0], "08 d7 00 04 00");
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
    t.b.radio.borrow_mut().ack = false;
    let before = t.b.radio.borrow().frames;
    t.fn_hold(BT1, 0.03);
    t.b.run_for(0.6);
    let after = t.b.radio.borrow().frames;
    assert!(after - before >= 41, "{} frames", after - before);
    // Back on USB to read them: in wireless mode USB is detached.
    t.fn_hold(USB, 0.03);
    t.log_should_show("configured", 2.0);
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
    bond_dongle(&mut t);
    t.b.host_present(DEVS_2G4, false);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    let seen = led_colours_over(&mut t, LED_2G4, 2.5);
    assert!(
        seen.contains(&RED) && seen.contains(&0) && !seen.contains(&GREEN),
        "{seen:06x?}"
    );
    // The dongle's host comes within reach: the module connects.
    t.b.host_present(DEVS_2G4, true);
    t.part_should_show(Part::Radio, "^-> 5b 32", 1.0);
    t.b.run_for(0.1);
    assert_eq!(t.led(LED_2G4), GREEN);
    let seen = led_colours_over(&mut t, LED_2G4, 1.6);
    assert!(!seen.contains(&RED), "{seen:06x?}");
    t.b.run_for(0.5);
    let seen = led_colours_over(&mut t, LED_2G4, 0.5);
    assert!(
        !seen.contains(&GREEN) && !seen.contains(&RED),
        "{seen:06x?}"
    );
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
    t.allow_errors();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 0);
    t.b.link_drop(None);
    t.b.run_for(35.0);
    should_be_asleep(&mut t, false, false);
    // Over the dongle with the link down, which the real one couldn't do:
    // the shortest way to change the setting 35 s into a drop.
    set_sleep_timeout(&mut t, 7);
    t.b.run_for(0.5);
    // Input (the Vial request isn't) restarted nothing: 35 s down already.
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    assert_eq!(t.b.errors(), ["radio: test: raw HID without a 2.4G link"]);
}

/// As the vendor's: with the link down, keys don't restart the 30 s (only
/// a device key would), so it sleeps 30 s after the link went down. The
/// host's resume frame wakes it from any sleep.
#[test]
fn typing_doesnt_hold_off_the_link_down_sleep_the_host_resuming_wakes_it() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.link_drop(None);
    t.b.run_for(10.0);
    tap(&mut t, KEY_A);
    t.b.run_for(10.0);
    tap(&mut t, KEY_A);
    t.b.run_for(10.5);
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    // The firmware's side only: asleep it told the module USB, which then
    // has no link to hear the host on.
    module_sends(&mut t, HOST_RESUME);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

/// The module's host state frame (0x60): 0 the host suspended, 1 it
/// resumed. Also with the setting "never". A long sleep doesn't trip the
/// watchdog.
#[test]
fn the_host_suspending_sleeps_it_at_once_its_resume_wakes_it() {
    let mut t = boot();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 0);
    t.b.dongle_host(false);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    // Asleep, the scan runs at 10 ms: the watchdog (2 s) stays quiet. A
    // reset would have woken it.
    t.b.run_for(10.0);
    should_be_asleep(&mut t, true, false);
    assert_eq!(t.b.resets, 0);
    t.b.dongle_host(true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_knob_wakes_it_and_the_turn_is_sent() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.dongle_host(false);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    knob_up(&mut t, 0.002);
    // A consumer report through the module: a3, volume up (e9).
    radio_should_get(&mut t, "a3 e9 00", 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

/// At 0% for a minute (M7a's timer) the module is told and the keyboard
/// sleeps; keys don't wake it (the columns stay released); the cable going
/// in (A7 high) does.
#[test]
fn a_flat_battery_sleeps_it_until_the_cable_goes_in() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.charger.level = 0.0;
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, true);
    key(&mut t, KEY_A, true);
    t.b.run_for(0.3);
    key(&mut t, KEY_A, false);
    should_be_asleep(&mut t, true, true);
    t.b.plug(true);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

/// Lower sleep stopped: only the cable and the UART are armed. A key
/// doesn't wake it; a frame from the module does, and is lost with the
/// clocks (`in_stop_mode_the_modules_uart_wakes_it`); the module sends it
/// again and it's read, and the keyboard stops again (the host resuming
/// doesn't end a flat battery's sleep); the cable going in ends it.
#[test]
fn in_stop_mode_a_flat_battery_stops_it_until_the_cable_goes_in() {
    let mut t = boot();
    stop_mode(&mut t, 0x53);
    t.hid_should_show(3, "08 d9 53 01", 1.0);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.charger.level = 0.0;
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(1.5);
    should_be_asleep(&mut t, true, true);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, false);
    // As the module would send it (the firmware told it USB: no link),
    // and again 10 ms later, unACKed.
    t.b.radio.borrow_mut().retransmit_ms = Some(10);
    let (lost, acks) = (t.b.chip.uart3.lost, t.b.radio.borrow().acks);
    module_sends(&mut t, HOST_RESUME);
    t.b.run_for(0.1);
    assert_eq!(t.b.chip.uart3.lost - lost, 3, "the frame that woke it");
    assert_eq!(
        t.b.radio.borrow().acks - acks,
        1,
        "the one sent again, read"
    );
    t.b.run_for(1.5);
    should_be_asleep(&mut t, true, true);
    should_be_stopped(&mut t, true);
    t.b.plug(true);
    t.b.run_for(0.5);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    stop_counts_should_be(&mut t, 2, 1);
}

/// From a reset stop mode stops only with the host gone: a suspended
/// host's resume would wake a stop and be lost with its frame, so asleep
/// for it the clocks run on.
#[test]
fn by_default_a_suspended_host_doesnt_stop_it() {
    let mut t = boot();
    t.raw_hid_send_and_run(&[0x08, 0xD9, 0x00]);
    t.hid_should_show(3, "08 d9 00 02", 1.0);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.dongle_host(false);
    t.b.run_for(40.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
    t.b.dongle_host(true);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    stop_counts_in_mode(&mut t, STOP_LINK, 0, 0);
    no_radio_errors(&t);
    t.no_errors();
}

/// The link lost (the computer off): asleep 30 s on, stopped a second
/// later. A key held wakes it, the dongle is selected again and, once the
/// host is back, the key goes out.
#[test]
fn by_default_a_lost_link_stops_it_and_a_held_key_is_sent() {
    let mut t = boot();
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.link_drop(None);
    t.b.run_for(30.5);
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(1.5);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, true);
    t.b.run_for(60.0);
    should_be_stopped(&mut t, true);
    t.b.host_present(DEVS_2G4, true);
    t.b.run_for(1.0);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    stop_counts_in_mode(&mut t, STOP_LINK, 1, 1);
    no_radio_errors(&t);
    t.no_errors();
}

/// Asleep idle first (1 min), then the computer shut down: stopped 30 s
/// after the link went. The module, not told to sleep, connects when the
/// host is back; its frame wakes the stop and is lost (the clocks come
/// back after it), so the keyboard stops again. A key then selects the
/// dongle again, the module announces the link, and the key goes out.
#[test]
fn by_default_asleep_idle_then_the_host_gone_stops_it() {
    let mut t = boot();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 1);
    t.b.plug(false);
    t.b.run_for(61.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
    t.b.link_drop(None);
    t.b.run_for(29.0);
    should_be_stopped(&mut t, false);
    t.b.run_for(2.0);
    should_be_stopped(&mut t, true);
    t.b.host_present(DEVS_2G4, true);
    t.b.run_for(2.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    no_radio_errors(&t);
    t.no_errors();
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
    // POST_LP: the trim from the information block, back.
    assert_eq!(t.b.word(ANCTL_TRIM), akko_sim::INFO_TRIM.1 & 0xF);
    stop_counts_should_be(&mut t, 1, 1);
    no_radio_errors(&t);
    // PRE_LP, POST_LP and the clocks' set-up again open ANCTL first.
    t.no_errors();
}

/// The stop diag's "woke" marks (wb32_hal::stop::Mark::Woke) set the LEDs
/// before the clocks are back, so GPIOB and GPIOC have no clock (RM 5.3.4)
/// and the writes go nowhere: on the chip, both LEDs on can't show.
/// A known firmware issue (retro review, 2026-10-30), pinned here until the
/// diag is fixed or dropped.
const DIAG_WOKE_UNCLOCKED: [&str; 2] = [
    "write to GPIOC at 0x40000818 = 0x200000 with its clock off: dropped",
    "write to GPIOB at 0x40000418 = 0x10000 with its clock off: dropped",
];

/// 0xD9 'D': stop mode with the LEDs marking how far a stop got
/// (src/stop.rs). Stopped: Caps on (C5 low), Win-lock off (B0 high); a key
/// still wakes it, and the device is selected again.
#[test]
fn with_stop_diag_on_caps_shows_a_stop_in_progress() {
    let mut t = boot();
    t.allow_errors();
    stop_mode(&mut t, 0x44);
    t.hid_should_show(3, "08 d9 44 01", 1.0);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.dongle_host(false);
    t.b.run_for(1.7);
    should_be_stopped(&mut t, true);
    assert!(caps_led(&mut t), "Caps on");
    assert!(t.pin_high(GPIOB_ODR, WIN_LOCK_LED), "Win-lock off");
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    key(&mut t, KEY_A, false);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    assert_eq!(t.b.errors(), DIAG_WOKE_UNCLOCKED);
}

/// 0xD9 'D' 63: both edges, pending cleared, no knob, EXTI events, the long
/// settle, the PLL restarted (src/stop.rs VARIANT_*). Byte 16 of the answer
/// echoes the bits.
#[test]
fn every_stop_diag_variant_at_once_still_stops_and_wakes_on_a_key() {
    let mut t = boot();
    t.allow_errors();
    t.raw_hid_send_and_run(&[0x08, 0xD9, 0x44, 63]);
    t.hid_should_show(3, "08 d9 44 01", 1.0);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.dongle_host(false);
    t.b.run_for(1.7);
    should_be_stopped(&mut t, true);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    radio_should_get(&mut t, A_DOWN, 1.0);
    key(&mut t, KEY_A, false);
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    vial_via_dongle(
        &mut t,
        &[0x08, 0xD9, 0x00],
        "08 d9 00 01 01 00 00 00 01 00 00 00 00 00 00 00 3f",
    );
    assert_eq!(t.b.errors(), DIAG_WOKE_UNCLOCKED);
}

/// A frame from the module ends the stop: its start bit is the falling
/// edge on C11, EXTI 11. The whole frame is lost: UART3 has no clock
/// until the HSE has started and the PLL locked (over 2 ms), and the frame
/// takes 0.26 ms. Not ACKed, the module sends it again (assumed, as the
/// keyboard does its own after 10 ms) and the radio reads it: the host
/// resuming wakes the keyboard.
#[test]
fn in_stop_mode_the_modules_uart_wakes_it() {
    let mut t = asleep_and_stopped();
    t.b.radio.borrow_mut().retransmit_ms = Some(10);
    let lost = t.b.chip.uart3.lost;
    t.b.dongle_host(true);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    assert_eq!(t.b.chip.uart3.lost - lost, 3, "the frame that woke it");
    assert!(
        t.b.radio
            .borrow()
            .log
            .iter()
            .any(|l| l == "-> 60 01 61 (again)")
    );
    t.b.run_for(0.3);
    should_be_stopped(&mut t, false);
    should_be_asleep(&mut t, false, false);
    stop_counts_should_be(&mut t, 1, 0);
}

#[test]
fn in_stop_mode_the_cable_wakes_it_and_it_doesnt_stop_with_the_cable_in() {
    let mut t = asleep_and_stopped();
    t.b.plug(true);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, false, false);
    t.b.dongle_host(false);
    t.b.run_for(2.0);
    should_be_asleep(&mut t, true, false);
    should_be_stopped(&mut t, false);
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
    t.b.radio.borrow_mut().ack_delay_ms = 4;
    for _ in 0..40 {
        knob_up(&mut t, 0.001);
    }
    t.b.run_for(2.0);
    let presses = t.b.log(Part::Radio)[t.radio..]
        .iter()
        .filter(|l| l.starts_with("<- a3 e9 00"))
        .count();
    println!("knob steps through the dongle: {presses} of 40");
    assert_eq!(presses, 40);
    no_radio_errors(&t);
}

/// Fn+E held 3 s with the module's model: it pairs (5B 31), the host takes
/// it (5B 32), and the module keeps the bond; keys then reach BT1.
#[test]
fn a_pairing_the_host_accepts_bonds_and_connects() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.fn_hold(BT1, 3.1);
    t.part_should_show(Part::Radio, "^-> 5b 31", 1.0);
    t.part_should_show(Part::Radio, "^-> 5b 32", 3.0);
    assert!(t.b.radio.borrow().bonded[1]);
    t.b.run_for(0.1);
    tap(&mut t, KEY_A);
    radio_should_get(&mut t, A_DOWN, 1.0);
    no_radio_errors(&t);
    t.no_errors();
}

/// The module's frames spoilt on the wire: CONNECTED loses a byte (the
/// keyboard drops the partial frame after 20 ms: a resync) and the raw
/// frame of a Vial request has a bad sum (a checksum error). Neither is
/// ACKed, and the module's second try is taken. The counters, the module's
/// version and battery and the device read back through the dongle (Vial
/// 0xD4).
#[test]
fn spoilt_module_frames_are_counted_and_the_second_try_is_taken() {
    let mut t = T::boot();
    t.b.radio.borrow_mut().version = 0x17;
    t.log_should_show("configured", 4.0);
    t.b.run_for(0.05);
    bond_dongle(&mut t);
    t.fn_hold(DONGLE, 0.03);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    {
        let mut r = t.b.radio.borrow_mut();
        r.retransmit_ms = Some(30);
        r.faults.extend([Fault::LostByte, Fault::BadSum]);
    }
    t.part_should_show(Part::Radio, r"^-> 5b 8d \(LostByte\)", 1.0);
    t.part_should_show(Part::Radio, r"^-> 5b 32 8d \(again\)", 0.1);
    radio_should_get(&mut t, ALL_UP, 1.0);
    let b = t.b.vial_via_dongle(&[0x08, 0xD4], 1.0).expect("no answer");
    let field = |i: usize| u16::from_le_bytes([b[2 + 2 * i], b[3 + 2 * i]]);
    assert_eq!((field(4), field(5)), (1, 1), "checksum errors, resyncs");
    assert_eq!(
        (b[21], b[22], b[23]),
        (100, 0x17, 6),
        "battery, version, device"
    );
    t.part_should_show(Part::Radio, r"\(BadSum\)", 0.0);
    no_radio_errors(&t);
}

/// E-1 (retro review 2026-10-30): a key held through the idle timeout. It
/// sleeps once, the keys released to the host, and stays asleep while the
/// key stays down: asleep, a row with a key down waits for it to come up,
/// as the vendor's EXTI arms both edges. The release is a change, and wakes
/// it.
#[test]
fn a_key_held_through_the_sleep_timeout_sleeps_once() {
    let mut t = boot();
    on_the_dongle(&mut t);
    set_sleep_timeout(&mut t, 1);
    key(&mut t, KEY_A, true);
    radio_should_get(&mut t, A_DOWN, 1.0);
    t.b.run_for(61.0);
    radio_should_get(&mut t, ALL_UP, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    t.b.run_for(5.0);
    should_be_asleep(&mut t, true, false);
    radio_should_not_get(&t, SEL_DONGLE);
    key(&mut t, KEY_A, false);
    radio_should_get(&mut t, SEL_DONGLE, 1.0);
    t.b.run_for(0.3);
    should_be_asleep(&mut t, false, false);
    no_radio_errors(&t);
}

/// C-1/D-1 (retro review): USB's Caps Lock doesn't follow the keys to the
/// dongle; there the host's lock LEDs are the module's (0 until it says
/// otherwise), as QMK's wireless driver.
#[test]
fn usbs_caps_lock_doesnt_follow_to_the_dongle() {
    let mut t = boot();
    // RMK's LED reader reads again a second after its first failed read.
    t.b.run_for(1.1);
    t.b.host.command(0x0203);
    t.b.run_for(0.05);
    assert!(caps_led(&mut t));
    on_the_dongle(&mut t);
    t.b.run_for(0.1);
    assert!(!caps_led(&mut t));
}

/// B-1 (retro review): the battery goes flat while asleep for the link (stop
/// mode off, so time runs). The cable going in wakes it, and the request
/// latched meanwhile doesn't put it straight back to sleep.
#[test]
fn a_flat_battery_latched_asleep_doesnt_outlast_the_cable() {
    let mut t = boot();
    stop_mode(&mut t, 1);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.link_drop(None);
    t.b.run_for(30.5);
    radio_should_get(&mut t, SEL_USB, 1.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    t.b.charger.level = 0.0;
    t.b.run_for(65.0);
    t.b.plug(true);
    t.b.run_for(1.0);
    should_be_asleep(&mut t, false, false);
    t.b.run_for(3.0);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

/// B-2 (retro review): the battery goes flat while asleep for the host: the
/// module is told and it's the flat battery's sleep (keys don't wake it);
/// the cable going in ends it.
#[test]
fn a_battery_flat_while_asleep_for_the_host_is_the_flat_sleep() {
    let mut t = boot();
    stop_mode(&mut t, 1);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.dongle_host(false);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, false);
    t.b.charger.level = 0.0;
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, true);
    key(&mut t, KEY_A, true);
    t.b.run_for(0.3);
    key(&mut t, KEY_A, false);
    should_be_asleep(&mut t, true, true);
    t.b.plug(true);
    t.b.run_for(1.0);
    should_be_asleep(&mut t, false, false);
    println!(
        "LOG {:#?}",
        t.b.log(Part::Radio)
            .iter()
            .filter(|l| l.starts_with("<-") && !l.contains("ack"))
            .collect::<Vec<_>>()
    );
}

/// E-3 (retro review): in the flat battery's sleep (stop mode off, so the
/// knob's lines stay armed) the knob's steps are dropped, not sent.
#[test]
fn in_the_flat_batterys_sleep_the_knob_sends_nothing() {
    let mut t = boot();
    stop_mode(&mut t, 1);
    on_the_dongle(&mut t);
    t.b.plug(false);
    t.b.charger.level = 0.0;
    t.b.run_for(62.0);
    radio_should_get(&mut t, SEL_USB, 5.0);
    t.b.run_for(0.5);
    should_be_asleep(&mut t, true, true);
    knob_up(&mut t, 0.002);
    t.b.run_for(0.5);
    t.part_not_since(Part::Radio, "^<- a3 ");
    should_be_asleep(&mut t, true, true);
}

/// D-2 (retro review 2026-10-30): a housekeeping deadline (the battery's 3 s
/// inquiry) that passes while the link is busy doesn't spin the radio task;
/// it polls the link every millisecond. Unanswered selects of BT1, one
/// every 0.3 s, keep the link busy across the deadline.
#[test]
fn a_deadline_passed_while_the_link_is_busy_doesnt_spin() {
    let mut t = boot();
    t.b.run_for(0.2);
    t.b.radio.borrow_mut().ack = false;
    for i in 0..12 {
        let insns = t.b.cpu.instructions;
        t.fn_tap(BT1, 1);
        t.b.run_for(0.21);
        let per_s = (t.b.cpu.instructions - insns) as f64 / 0.3;
        assert!(per_s < MAX_INSNS_BUSY_LINK, "window {i}: {per_s}");
    }
}

/// B-6 (retro review 2026-10-30): a host state frame while USB is selected
/// isn't about the host the keys go to (the vendor clears it on USB). A
/// suspend heard on USB put it to sleep as Fn+Y switched to 2.4G, until
/// the keys came up: the lights went off while Y was held.
#[test]
fn a_host_suspend_heard_on_usb_doesnt_sleep_it_on_the_dongle() {
    let mut t = boot();
    module_sends(&mut t, (0x60, 0x00));
    t.b.run_for(0.1);
    bond_dongle(&mut t);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(DONGLE.0, DONGLE.1, true);
    for ms in 0..300 {
        t.b.run_for(0.001);
        assert!(
            t.b.word(GPIOA_ODR) & LED_BOOST != 0,
            "the lights off {ms} ms into Fn+Y"
        );
    }
    t.key(DONGLE.0, DONGLE.1, false);
    t.key(5, 10, false);
    t.b.run_for(1.0);
    should_be_asleep(&mut t, false, false);
}
