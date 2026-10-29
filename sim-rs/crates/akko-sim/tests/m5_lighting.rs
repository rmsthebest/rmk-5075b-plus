//! ../../sim/tests/m5_lighting.robot: the RGB lighting of the RMK image
//! (akko-rmk). The LED chain is the WS2812 decoder on SPIM2, which logs any
//! malformed frame as an error. Lighting starts once RMK's storage is up:
//! the first frame goes out about 2.8 s after a reset with a blank flash.
//!
//! The effects themselves are checked frame for frame against QMK's and the
//! vendor's C on the host (tools/rgbref); these tests check the firmware
//! around them: the driver, the frame timing, power, that keys reach the
//! board, and what it keeps. The side lights (LEDs 84-105) run the vendor's
//! rainbow swirl by default; the USB key's LED (37) shows white for 2 s
//! once USB is configured; most RGB keys confirm with a 0.5 s black blink.

mod common;

use akko_sim::Part;
use akko_sim::usb_host::cmd;
use common::*;

const RGB_MOD: (u8, u8) = (0, 13);
const RGB_TOG: (u8, u8) = (3, 9);
const RGB_VAI: (u8, u8) = (4, 13);
const RGB_VAD: (u8, u8) = (5, 13);
const RGB_SAI: (u8, u8) = (5, 12);
const RGB_SAD: (u8, u8) = (5, 14);
const RL_MOD: (u8, u8) = (5, 0);
const DONGLE: (u8, u8) = (2, 6);
/// The battery's GET_REPORT: input report 5 on the composite interface.
const GET_BATTERY: u32 = cmd::GET_REPORT | 5 << 8 | 1 << 16;

fn boot_to_lighting() -> T {
    let mut t = T::boot();
    t.b.run_for(2.5);
    for _ in 0..20 {
        if t.led_frames() > 0 {
            break;
        }
        t.b.run_for(0.05);
    }
    assert!(t.led_frames() > 0, "no LED frame by 3.5 s");
    // Past the USB key's 2 s of white after USB is configured.
    t.log_should_show("configured", 4.0);
    t.b.run_for(2.1);
    t
}

/// A system reset, the flash kept; on past the USB key's white.
fn reset_and_boot(t: &mut T) {
    t.b.system_reset();
    t.log_should_show("configured", 6.0);
    t.b.run_for(2.1);
}

/// The largest channel of LEDs 1-83: the brightness, for the default
/// effect (saturation 255).
fn brightest_key_led(t: &T) -> u32 {
    t.led_colours()[1..84]
        .iter()
        .map(|&c| (c >> 16).max((c >> 8) & 0xFF).max(c & 0xFF))
        .max()
        .unwrap()
}

/// A lighting save landing in sector `n` (0x20000 + n * 4 KB), and nothing
/// written outside RMK's and the lighting's sectors.
fn saved_to_sector(t: &mut T, n: u32) {
    t.part_should_show(Part::Nor, &format!("^PP 0x02{n}200"), 4.0);
    let re = regex::Regex::new(r"^(CE|BE|(SE|PP) 0x0[03-7])").unwrap();
    let bad: Vec<String> =
        t.b.log(Part::Nor)
            .into_iter()
            .filter(|l| re.is_match(l))
            .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

fn all_leds_should_be(t: &T, colour: u32) {
    let others: Vec<usize> = t
        .led_colours()
        .iter()
        .enumerate()
        .filter(|(_, c)| **c != colour)
        .map(|(i, _)| i)
        .collect();
    assert!(others.is_empty(), "LEDs not {colour:06x}: {others:?}");
}

/// LED 0's colour and the LED power, every 20 ms for `secs`: (colour,
/// power on, other key LEDs lit).
fn led_0_over(t: &mut T, secs: f64) -> Vec<(u32, bool, bool)> {
    (0..(secs * 50.0) as usize)
        .map(|_| {
            t.b.run_for(0.02);
            let c = t.led_colours();
            (c[0], t.led_power(), c[1..84].iter().any(|&x| x != 0))
        })
        .collect()
}

/// Cycle left-right at QMK's defaults (hue 0, saturation 255, brightness 84
/// = the board's maximum): every key LED at hue x - t for one effect time
/// t, and t moves on. The side lights swirl (hue 11 i + h for light i,
/// mirrored), 3 hue steps every 20 ms.
#[test]
fn the_leds_show_qmks_default_effect() {
    let mut t = boot_to_lighting();
    let c = t.led_colours();
    let (t1, h1) = (cycle_time(&c), swirl_hue(&c));
    t.b.run_for(0.2);
    let c = t.led_colours();
    let (t2, h2) = (cycle_time(&c), swirl_hue(&c));
    assert_ne!(t1, t2);
    assert_ne!(h1, h2);
    t.no_led_errors();
    assert!(t.led_power());
    t.no_errors();
}

#[test]
fn a_frame_goes_out_every_16_ms_while_the_colours_change() {
    let mut t = boot_to_lighting();
    let a = t.led_frames();
    t.b.run_for(1.0);
    let n = t.led_frames() - a;
    assert!((55..=63).contains(&n), "{n} frames in 1 s");
    t.no_led_errors();
}

/// The first register writes, through the first frame, against QMK's
/// (sim/golden/qmk_spim2_init.txt).
#[test]
fn spim2_is_set_up_and_driven_as_qmk_does() {
    let golden = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../sim/golden/qmk_spim2_init.txt"
    ))
    .unwrap();
    let golden: Vec<&str> = golden.trim().lines().collect();
    let mut t = T::boot();
    t.b.chip.writes = Some(Vec::new());
    t.b.run_for(3.5);
    let ours: Vec<String> =
        t.b.chip
            .writes
            .iter()
            .flatten()
            .map(|w| w.to_string())
            .filter(|l| l.starts_with("SPIM2"))
            .take(golden.len())
            .collect();
    assert_eq!(ours, golden);
}

/// rgb_matrix_hs_devs_indicator: 2 s of white on LED 37.
#[test]
fn the_usb_key_shows_white_once_usb_is_configured() {
    let mut t = T::boot();
    t.log_should_show("configured", 4.0);
    for _ in 0..40 {
        if t.led_frames() > 0 {
            break;
        }
        t.b.run_for(0.05);
    }
    t.b.run_for(0.1);
    assert_eq!(t.led_colours()[37], 0xFFFFFF);
    t.b.run_for(2.1);
    cycle_time(&t.led_colours());
    t.no_led_errors();
}

/// The side lights go off and on with the rest.
#[test]
fn rgb_toggle_turns_the_leds_and_their_supply_off_and_on() {
    let mut t = boot_to_lighting();
    t.fn_tap(RGB_TOG, 1);
    t.b.run_for(0.05);
    assert_eq!(lit(&t.led_colours()), Vec::<usize>::new());
    assert!(!t.led_power());
    t.fn_tap(RGB_TOG, 1);
    t.b.run_for(0.05);
    let c = t.led_colours();
    cycle_time(&c);
    swirl_hue(&c);
    assert!(t.led_power());
    t.no_led_errors();
    t.no_errors();
}

/// rgb_blink_dir: every LED black for 0.5 s.
#[test]
fn the_rgb_keys_confirm_with_a_black_blink() {
    let mut t = boot_to_lighting();
    t.fn_tap(RGB_MOD, 1);
    t.b.run_for(0.1);
    all_leds_should_be(&t, 0);
    t.b.run_for(0.5);
    let c = t.led_colours();
    assert!(!lit_keys(&c).is_empty());
    swirl_hue(&c);
}

/// The side lights keep their own brightness (120); at 0 the vendor's code
/// cuts the LEDs' supply anyway.
#[test]
fn brightness_steps_by_21_and_the_supply_goes_off_at_0() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_VAD, 1);
    cycle_left_right_time(&t.led_colours(), 255, 63);
    t.fn_tap_and_wait(RGB_VAD, 3);
    assert_eq!(lit_keys(&t.led_colours()), Vec::<usize>::new());
    assert!(!t.led_power());
    t.fn_tap_and_wait(RGB_VAI, 1);
    cycle_left_right_time(&t.led_colours(), 255, 21);
    assert!(t.led_power());
    t.fn_tap_and_wait(RGB_VAI, 5);
    cycle_left_right_time(&t.led_colours(), 255, 84);
    t.no_led_errors();
}

/// RGB_MOD steps through the vendor's list (13, 15, 16, 24, 25, 26, 29,
/// 37, 33, ...): eight steps from cycle left-right is solid reactive simple
/// (33). LEDs light where keys are hit and fade in about 0.4 s. T (2, 5)
/// then lights LED 39 alone of the keys. The Win/Mac switch's taps aren't
/// hits.
#[test]
fn a_reactive_effect_lights_the_key_pressed() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_MOD, 8);
    assert_eq!(lit_keys(&t.led_colours()), Vec::<usize>::new());
    t.key(2, 5, true);
    t.b.run_for(0.05);
    assert_eq!(lit_keys(&t.led_colours()), [39]);
    t.key(2, 5, false);
    t.b.run_for(0.6);
    assert_eq!(lit_keys(&t.led_colours()), Vec::<usize>::new());
    t.no_led_errors();
}

/// RGB_SAI / RGB_SAD step through the vendor's five colours, the last
/// white (saturation 0), and blink only when they move.
#[test]
fn saturation_keys_step_the_effects_colour() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_SAI, 4);
    cycle_left_right_time(&t.led_colours(), 0, 84);
    t.fn_tap(RGB_SAI, 1);
    t.b.run_for(0.1);
    cycle_left_right_time(&t.led_colours(), 0, 84);
    t.fn_tap_and_wait(RGB_SAD, 1);
    cycle_time(&t.led_colours());
}

/// RL_MOD: swirl, rainbow mood, breathing, static red, the static hues 40
/// to 200, white, off, and round to the swirl.
#[test]
fn fn_lctrl_steps_the_side_lights() {
    let mut t = boot_to_lighting();
    t.fn_tap(RL_MOD, 1);
    t.b.run_for(0.3);
    assert_ne!(side_colour(&t.led_colours()), 0);
    t.fn_tap(RL_MOD, 2);
    t.b.run_for(0.1);
    assert_eq!(side_colour(&t.led_colours()), hsv_to_rgb(0, 255, 120));
    for h in [40, 80, 120, 160, 200] {
        t.fn_tap(RL_MOD, 1);
        t.b.run_for(0.1);
        assert_eq!(
            side_colour(&t.led_colours()),
            hsv_to_rgb(h, 255, 120),
            "hue {h}"
        );
    }
    t.fn_tap(RL_MOD, 1);
    t.b.run_for(0.1);
    assert_eq!(side_colour(&t.led_colours()), hsv_to_rgb(0, 0, 120));
    t.fn_tap(RL_MOD, 1);
    t.b.run_for(0.1);
    let c = t.led_colours();
    assert_eq!(side_colour(&c), 0);
    cycle_time(&c);
    assert!(t.led_power());
    t.fn_tap(RL_MOD, 1);
    t.b.run_for(0.1);
    swirl_hue(&t.led_colours());
    t.no_led_errors();
}

/// RP_P0 and RP_END aren't on the default keymap: Vial puts them on Fn+Y
/// and Fn+T. RP_P0 plays channel 0 (green keys); RP_END starts recording,
/// which clears it; A then turns red and doesn't reach the host. RP_END
/// saves it; RGB_MOD leaves playback; RP_P0 shows the recording, and A
/// types again.
#[test]
fn recording_takes_key_presses_and_plays_them_back() {
    let mut t = boot_to_lighting();
    t.raw_hid_send_and_run(&[0x05, 1, 2, 6, 0x7E, 0x05]);
    t.raw_hid_send_and_run(&[0x05, 1, 2, 5, 0x7E, 0x08]);
    t.fn_tap((2, 6), 1);
    t.b.run_for(0.1);
    let c = t.led_colours();
    assert_eq!(lit_keys(&c), [14, 42, 45, 46, 47, 60, 81, 82, 83]);
    assert_eq!(c[46], hsv_to_rgb(85, 255, 84));
    t.fn_tap((2, 5), 1);
    t.b.run_for(0.1);
    assert_eq!(lit_keys(&t.led_colours()), Vec::<usize>::new());
    t.key(3, 1, true);
    t.b.run_for(0.05);
    t.key(3, 1, false);
    t.b.run_for(0.1);
    t.hid_not_since(1, "00 00 04");
    let c = t.led_colours();
    assert_eq!(lit_keys(&c), [46]);
    assert_eq!(c[46], hsv_to_rgb(0, 255, 84));
    t.fn_tap((2, 5), 1);
    t.fn_tap_and_wait(RGB_MOD, 1);
    cycle_time(&t.led_colours());
    t.fn_tap((2, 6), 1);
    t.b.run_for(0.1);
    assert_eq!(lit_keys(&t.led_colours()), [46]);
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.no_led_errors();
}

/// Fn+` held 1 s does nothing. Held 3 s the lighting resets and blinks grey
/// (0x10) three times; on release RMK erases its storage and reboots, so a
/// keymap change made through Vial is gone.
#[test]
fn ee_clr_resets_only_after_a_3_s_hold() {
    let mut t = boot_to_lighting();
    // dynamic_keymap_set_keycode(layer 0, row 3, col 1) = B.
    t.raw_hid_send_and_run(&[0x05, 0, 3, 1, 0x00, 0x05]);
    t.b.run_for(0.5);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(1, 0, true);
    t.b.run_for(1.0);
    t.key(1, 0, false);
    t.b.run_for(0.03);
    t.key(5, 10, false);
    t.b.run_for(0.5);
    t.hid_not_since(1, "00 00 35");
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 05 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.b.run_for(0.1);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(1, 0, true);
    t.b.run_for(3.05);
    let mut grey = false;
    for _ in 0..12 {
        t.b.run_for(0.1);
        if t.led_colours().iter().all(|&c| c == 0x101010) {
            grey = true;
            break;
        }
    }
    assert!(grey, "no grey blink after the 3 s hold");
    t.key(1, 0, false);
    t.b.run_for(0.03);
    t.key(5, 10, false);
    t.log_should_show("configured", 6.0);
    t.b.run_for(0.1);
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
}

/// Fn+Down dims the default effect from 84 to 63. The save goes to the
/// first lighting sector 3 s after the last key, and a reset brings it back.
#[test]
fn lighting_settings_survive_a_reset() {
    let mut t = boot_to_lighting();
    assert_eq!(brightest_key_led(&t), 84);
    t.fn_tap_and_wait(RGB_VAD, 1);
    assert_eq!(brightest_key_led(&t), 63);
    saved_to_sector(&mut t, 0);
    reset_and_boot(&mut t);
    assert_eq!(brightest_key_led(&t), 63);
    t.no_led_errors();
    t.no_errors();
}

/// Two saves alternate sectors; the second one damaged (as by a power cut
/// while writing), the first is what loads.
#[test]
fn a_torn_lighting_save_keeps_the_one_before() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_VAD, 1);
    saved_to_sector(&mut t, 0);
    t.fn_tap_and_wait(RGB_VAD, 1);
    assert_eq!(brightest_key_led(&t), 42);
    saved_to_sector(&mut t, 1);
    t.b.nor.borrow_mut().set_word(0x21100, 0x1234_5678);
    reset_and_boot(&mut t);
    assert_eq!(brightest_key_led(&t), 63);
    t.no_led_errors();
}

/// A saved dimmer setting is gone after EE_CLR's reboot: the defaults are
/// saved in its place (into the other sector).
#[test]
fn ee_clr_resets_the_saved_lighting() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_VAD, 1);
    saved_to_sector(&mut t, 0);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(1, 0, true);
    t.b.run_for(3.05);
    t.part_should_show(Part::Nor, "^SE 0x021000", 1.0);
    t.key(1, 0, false);
    t.b.run_for(0.03);
    t.key(5, 10, false);
    t.log_should_show("configured", 6.0);
    t.b.run_for(2.1);
    assert_eq!(brightest_key_led(&t), 84);
    t.no_led_errors();
}

/// HS_DIR (Fn+W), as the vendor's: A types Left and Left types A, Up types
/// W; saved, so it survives a reset; Fn+W again undoes it.
#[test]
fn fn_w_swaps_wasd_and_the_arrows_and_its_kept() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait((2, 2), 1);
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 50 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.b.run_for(0.05);
    t.key(5, 12, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(5, 12, false);
    saved_to_sector(&mut t, 0);
    reset_and_boot(&mut t);
    t.key(4, 13, true);
    t.hid_should_show(1, "00 00 1a 00 00 00 00 00", 1.0);
    t.key(4, 13, false);
    t.b.run_for(0.05);
    t.fn_tap_and_wait((2, 2), 1);
    t.key(3, 3, true);
    t.hid_should_show(1, "00 00 07 00 00 00 00 00", 1.0);
    t.key(3, 3, false);
    t.no_led_errors();
}

/// hs_factory_scan: Left Ctrl held as the keyboard starts lights every LED
/// red, then green, blue and white (80), 4 s each; then the lighting is as
/// before.
#[test]
fn left_ctrl_at_power_up_runs_the_led_test() {
    let mut t = T::boot();
    t.key(5, 0, true);
    t.b.run_for(2.5);
    all_leds_should_be(&t, 0x500000);
    t.key(5, 0, false);
    t.b.run_for(3.5);
    all_leds_should_be(&t, 0x005000);
    t.b.run_for(4.0);
    all_leds_should_be(&t, 0x000050);
    t.b.run_for(4.0);
    all_leds_should_be(&t, 0x505050);
    t.b.run_for(4.0);
    assert_eq!(brightest_key_led(&t), 84);
    t.no_led_errors();
}

/// VialRGB over VIA's custom values: the protocol and the maximum
/// brightness (84); the default mode, cycle left-right (Vial's 14) at speed
/// 153; the supported ids after 40. Setting solid colour (2), green at 42,
/// shows on the keys and is saved.
#[test]
fn vials_lighting_tab_reads_sets_and_saves_the_effect() {
    let mut t = boot_to_lighting();
    t.raw_hid_send_and_run(&[0x08, 0x40]);
    t.hid_should_show(3, "08 40 01 00 54", 1.0);
    t.raw_hid_send_and_run(&[0x08, 0x41]);
    t.hid_should_show(3, "08 41 0e 00 99 00 ff 54", 1.0);
    t.raw_hid_send_and_run(&[0x08, 0x42, 40, 0]);
    t.hid_should_show(3, "08 42 29 00 2a 00 2b 00 2c 00 ff ff", 1.0);
    t.raw_hid_send_and_run(&[0x07, 0x41, 0x02, 0x00, 0x80, 85, 255, 42]);
    t.b.run_for(0.1);
    let c = t.led_colours();
    let want = hsv_to_rgb(85, 255, 42);
    let others: Vec<usize> = (1..84).filter(|&i| c[i] != want).collect();
    assert!(others.is_empty(), "{others:?}");
    t.raw_hid_send_and_run(&[0x08, 0x41]);
    t.hid_should_show(3, "08 41 02 00 80 55 ff 2a", 1.0);
    saved_to_sector(&mut t, 0);
    reset_and_boot(&mut t);
    t.raw_hid_send_and_run(&[0x08, 0x41]);
    t.hid_should_show(3, "08 41 02 00 80 55 ff 2a", 1.0);
    t.no_led_errors();
}

/// Fn+LWin (GU_TOGG) in Windows mode: the Win-lock LED (B0, active low)
/// comes on and LWin types nothing. It's saved: after a reset it's still
/// on; GU_TOGG again gives LWin back. Vial reads the key as QMK's GU_TOGG
/// (0x700B).
#[test]
fn gu_togg_turns_off_the_windows_keys_and_lights_win_lock() {
    let mut t = boot_to_lighting();
    t.raw_hid_send_and_run(&[0x04, 1, 5, 1]);
    t.hid_should_show(3, "04 01 05 01 70 0b", 1.0);
    assert!(t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    t.fn_tap((5, 1), 1);
    assert!(!t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    t.key(5, 1, true);
    t.b.run_for(0.05);
    t.key(5, 1, false);
    t.b.run_for(0.05);
    t.hid_not_since(1, "08");
    saved_to_sector(&mut t, 0);
    reset_and_boot(&mut t);
    assert!(!t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    t.fn_tap((5, 1), 1);
    assert!(t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    t.key(5, 1, true);
    t.hid_should_show(1, "08 00 00 00 00 00 00 00", 1.0);
    t.key(5, 1, false);
    t.no_led_errors();
}

/// Right Ctrl is Right Ctrl until Fn+RCtrl (HS_CT_A) is held 3 s; a black
/// blink confirms, and then it sends App (0x65).
#[test]
fn hs_ct_a_held_3_s_makes_right_ctrl_send_app() {
    let mut t = boot_to_lighting();
    t.key(5, 11, true);
    t.hid_should_show(1, "10 00 00 00 00 00 00 00", 1.0);
    t.key(5, 11, false);
    t.b.run_for(0.05);
    t.key(5, 10, true);
    t.b.run_for(0.03);
    t.key(5, 11, true);
    t.b.run_for(2.9);
    let lit_count = |t: &T| t.led_colours()[1..84].iter().filter(|&&c| c != 0).count();
    assert!(lit_count(&t) > 0, "dark before the 3 s");
    let mut black = false;
    for _ in 0..6 {
        t.b.run_for(0.05);
        if lit_count(&t) == 0 {
            black = true;
            break;
        }
    }
    assert!(black, "no black blink after the 3 s hold");
    t.key(5, 11, false);
    t.b.run_for(0.03);
    t.key(5, 10, false);
    t.b.run_for(0.05);
    t.key(5, 11, true);
    t.hid_should_show(1, "00 00 65 00 00 00 00 00", 1.0);
    t.key(5, 11, false);
    t.no_led_errors();
}

/// B9 is high while the lighting is on and the cable sense (A7) is low;
/// the cable (A7 high) or Fn+L turns it off.
#[test]
fn the_aux_led_shows_lighting_on_without_a_cable() {
    let mut t = boot_to_lighting();
    assert!(!t.pin_high(GPIOB_ODR, AUX_LED), "the cable is in");
    t.b.plug(false);
    t.b.run_for(0.05);
    assert!(t.pin_high(GPIOB_ODR, AUX_LED));
    t.b.plug(true);
    t.b.run_for(0.05);
    assert!(!t.pin_high(GPIOB_ODR, AUX_LED));
    t.b.plug(false);
    t.b.run_for(0.05);
    assert!(t.pin_high(GPIOB_ODR, AUX_LED));
    t.fn_tap_and_wait(RGB_TOG, 1);
    assert!(!t.pin_high(GPIOB_ODR, AUX_LED));
    t.no_led_errors();
}

/// LED 0 (no key over it) is dark at a good level; on the battery at 15%
/// or less it blinks red, 250 ms on and off; with the cable in (A7 high)
/// the charger pulls A15 low and it's red, and dark again once the battery
/// is full (A15 high). Fn+G hides it.
#[test]
fn led_0_shows_the_battery_blinking_when_low_red_while_charging() {
    let mut t = boot_to_lighting();
    assert!(led_0_over(&mut t, 0.6).iter().all(|s| s.0 == 0));
    t.b.plug(false);
    t.set_battery(12);
    let mut colours: Vec<u32> = led_0_over(&mut t, 1.2).iter().map(|s| s.0).collect();
    colours.sort();
    colours.dedup();
    assert_eq!(colours, [0, 0xFF0000]);
    t.b.plug(true);
    t.b.run_for(0.1);
    assert!(led_0_over(&mut t, 0.6).iter().all(|s| s.0 == 0xFF0000));
    t.fn_tap((3, 5), 1);
    assert!(led_0_over(&mut t, 0.6).iter().all(|s| s.0 == 0));
    t.fn_tap((3, 5), 1);
    t.b.charger.level = 100.0;
    t.b.run_for(0.1);
    assert!(led_0_over(&mut t, 0.6).iter().all(|s| s.0 == 0));
    t.no_led_errors();
}

/// While Fn+Space is held the keys go dark but for a bar on LEDs 16-25 (the
/// number row), a LED per 10%: green from 50%, yellow from 30%, red below.
/// On USB too, and with the lighting off, which powers the LEDs just for it.
#[test]
fn fn_space_shows_the_battery_level() {
    let mut t = boot_to_lighting();
    let bar = |t: &mut T| {
        t.key(5, 10, true);
        t.b.run_for(0.03);
        t.key(5, 6, true);
        t.b.run_for(0.1);
        let c = t.led_colours();
        let power = t.led_power();
        t.key(5, 6, false);
        t.b.run_for(0.03);
        t.key(5, 10, false);
        t.b.run_for(0.1);
        (c, power)
    };
    for (percent, n, colour) in [(42, 4, 0x969600), (80, 8, 0x009600), (7, 1, 0x960000)] {
        t.set_battery(percent);
        let (c, _) = bar(&mut t);
        let mut want = vec![colour; n];
        want.resize(10, 0);
        assert_eq!(c[16..26], want, "{percent}%");
        assert!(
            c[1..16].iter().chain(&c[26..84]).all(|&x| x == 0),
            "{percent}%"
        );
    }
    // (Above 15%, or the low-battery flash would power the LEDs too.)
    t.set_battery(35);
    t.fn_tap_and_wait(RGB_TOG, 1);
    assert!(!t.led_power());
    let (c, power) = bar(&mut t);
    assert!(power);
    let mut want = vec![0x969600; 3];
    want.resize(10, 0);
    assert_eq!(c[16..26], want);
    assert!(!t.led_power());
    t.no_led_errors();
}

/// The lighting off (Fn+L), LED power is off; at 15% or less, every 5 s the
/// LEDs are powered for about 190 ms, LED 0 red for about 150 ms of it,
/// nothing else lit.
#[test]
fn with_the_lights_off_a_low_battery_flashes_led_0() {
    let mut t = boot_to_lighting();
    t.fn_tap_and_wait(RGB_TOG, 1);
    // On the battery, so on the dongle: with the cable in it charges, which
    // shows no flash, and on USB with the cable out the host's suspend
    // turns the lighting off after 7 s.
    t.b.radio.borrow_mut().bonded[akko_sim::radio::DEVS_2G4] = true;
    t.fn_hold(DONGLE, 0.03);
    t.b.run_for(0.5);
    t.b.plug(false);
    t.set_battery(10);
    let seen = led_0_over(&mut t, 10.5);
    let red: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter(|(_, s)| s.0 == 0xFF0000)
        .map(|(i, _)| i)
        .collect();
    let powered = seen.iter().filter(|s| s.1).count();
    let bad: Vec<usize> = red
        .iter()
        .copied()
        .filter(|&i| !seen[i].1 || seen[i].2)
        .collect();
    assert!(
        bad.is_empty(),
        "red without power, or with other LEDs lit: {bad:?}"
    );
    // Two flashes of ~150 ms, 5 s apart, sampled every 20 ms.
    assert!((12..=18).contains(&red.len()), "{red:?}");
    assert!(
        red.len() < powered && powered <= 24,
        "{powered} powered samples"
    );
    assert!(red[red.len() - 1] - red[0] > 200, "one flash only");
    t.no_led_errors();
}

/// As the vendor's housekeeping: on USB the Caps Lock LED is off unless USB
/// is active, the Win-lock LED off while suspended; both come back when the
/// host resumes.
#[test]
fn on_usb_caps_lock_and_win_lock_are_off_while_the_host_is_suspended() {
    let mut t = boot_to_lighting();
    t.fn_tap((5, 1), 1);
    assert!(!t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    t.b.host.command(cmd::HID_LEDS | 2 << 8);
    t.b.run_for(0.1);
    assert!(!t.pin_high(GPIOC_ODR, CAPS_LED));
    t.b.host.command(cmd::SUSPEND);
    t.b.run_for(0.5);
    assert!(t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    assert!(t.pin_high(GPIOC_ODR, CAPS_LED));
    t.b.host.command(cmd::RESUME);
    t.b.run_for(0.5);
    assert!(!t.pin_high(GPIOB_ODR, WIN_LOCK_LED));
    assert!(!t.pin_high(GPIOC_ODR, CAPS_LED));
}

/// QMK's suspend_power_down after USB_POWER_DOWN_DELAY: the host suspended
/// for 7 s turns the LED power off (the MCU doesn't sleep on USB); its
/// resume turns it back on.
#[test]
fn on_usb_the_lights_go_off_7_s_after_the_host_suspends() {
    let mut t = boot_to_lighting();
    t.b.host.command(cmd::SUSPEND);
    t.b.run_for(6.5);
    t.host_not_since("device did not detect suspend");
    assert!(t.led_power());
    t.b.run_for(0.8);
    assert!(!t.led_power());
    t.b.host.command(cmd::RESUME);
    t.b.run_for(0.3);
    assert!(t.led_power());
    t.no_led_errors();
}

/// The host enables remote wakeup and suspends, as Linux does; a key press
/// drives resume for 1-15 ms (QMK's 2 ms), and the next key types.
#[test]
fn a_key_wakes_a_host_that_allows_remote_wakeup() {
    let mut t = boot_to_lighting();
    t.b.host.command(cmd::SUSPEND_WAKEUP);
    t.b.run_for(0.1);
    t.host_not_since("HOST ERROR");
    t.key(3, 1, true);
    t.b.run_for(0.03);
    t.key(3, 1, false);
    let line = t.log_matching(r"remote wakeup, RESUME held \d+ ms", 1.0);
    let ms: u32 = regex::Regex::new(r"held (\d+) ms")
        .unwrap()
        .captures(&line)
        .unwrap()[1]
        .parse()
        .unwrap();
    assert!((1..=15).contains(&ms), "RESUME held {ms} ms");
    t.b.run_for(0.1);
    t.key(3, 1, true);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    t.key(3, 1, false);
    t.hid_should_show(1, "00 00 00 00 00 00 00 00", 1.0);
    t.host_not_since("HOST ERROR");
    t.no_errors();
}

/// M7e: a GET_REPORT of the battery before the module has told the level
/// gets a stand-in, 100% and not charging, not a stall (a failed first read
/// makes upower ignore the battery until it restarts); the first real
/// level replaces it. The module here ACKs nothing at first, so the
/// inquiry never goes out.
#[test]
fn before_the_module_tells_the_level_the_host_reads_100() {
    let mut t = T::boot();
    t.b.radio.borrow_mut().ack = false;
    t.log_should_show("configured", 4.0);
    t.b.run_for(0.5);
    t.b.host.command(GET_BATTERY);
    t.log_matching(r"GET_REPORT\(input 5\) -> 3 bytes: 05 64 00", 1.0);
    t.hid_not_since(2, "05");
    t.set_battery(57);
    t.hid_matching(2, "05 39 0[01]", 4.0);
    t.b.host.command(GET_BATTERY);
    t.log_matching(r"GET_REPORT\(input 5\) -> 3 bytes: 05 39 0[01]", 1.0);
}

/// M7e: report 0x05 on the composite interface (RMK's hid.rs
/// BatteryReport): the level the module reports, 0-100, and charging (the
/// cable in, A7 high, and not full, A15 low). Sent a second after USB is
/// configured, then on each change. A GET_REPORT of it (Linux asks as soon
/// as it sees the battery) gets the level as soon as the module has told
/// it; before, a stall.
#[test]
fn on_usb_the_battery_is_reported_to_the_host() {
    let mut t = T::boot();
    t.log_should_show("configured", 4.0);
    // As Linux, as soon as it sees the battery: answered already, the
    // module asked right after its post-init (100 ms), not 3 s on.
    t.b.host.command(GET_BATTERY);
    t.log_matching(r"GET_REPORT\(input 5\) -> 3 bytes: 05 64 0[01]", 1.0);
    // Below full, on the cable: charging (the report's second byte).
    t.set_battery(57);
    t.hid_should_show(2, "05 39 01", 2.0);
    t.b.host.command(GET_BATTERY);
    t.log_matching(r"GET_REPORT\(input 5\) -> 3 bytes: 05 39 01", 1.0);
    t.set_battery(58);
    t.hid_should_show(2, "05 3a 01", 1.0);
    t.b.charger.level = 100.0;
    t.hid_should_show(2, "05 64 00", 4.0);
    t.no_errors();
}
