//! The board around the chip (src/parts.rs, the cable, B1's pull-up, the
//! LED supply): what the firmware sees when they change, and that it
//! drives them as the board needs.

mod common;

use akko_sim::Part;
use akko_sim::parts::{CABLE, CHARGING};
use akko_sim::usb_host::UsbEvent;
use common::*;

const DONGLE: (u8, u8) = (2, 6);
const USB: (u8, u8) = (2, 7);
const RGB_TOG: (u8, u8) = (3, 9);
const KEY_A: (u8, u8) = (3, 1);

/// HID reports on `ep` since the cursor whose bytes are `want`.
fn reports(t: &T, ep: u8, want: &str) -> usize {
    t.b.host.events()[t.usb..]
        .iter()
        .filter(|e| matches!(e, UsbEvent::Hid { ep: e, data } if *e == ep && akko_sim::usb_host::hex(data) == want))
        .count()
}

/// The cable out: A7 low and the bus idle, which the device sees as a
/// suspend (the lighting goes off 7 s on, USB_POWER_DOWN_DELAY); back in,
/// A7 high and the host enumerates it again, and keys reach it.
#[test]
fn the_cable_out_suspends_usb_and_back_in_it_enumerates() {
    let mut t = T::boot_and_enumerate();
    assert!(t.b.chip.gpio.pin_level(CABLE));
    t.b.plug(false);
    t.log_should_show("cable unplugged", 0.01);
    assert!(!t.b.chip.gpio.pin_level(CABLE));
    t.b.run_for(6.5);
    assert!(t.led_power(), "lights on before the 7 s");
    t.b.run_for(1.0);
    assert!(!t.led_power(), "lights off 7 s after the suspend");
    t.b.plug(true);
    assert!(t.b.chip.gpio.pin_level(CABLE));
    t.log_should_show("configured", 2.0);
    t.tap(KEY_A.0, KEY_A.1);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    assert_eq!(t.b.host_errors(), Vec::<String>::new());
}

/// Fn+Y raises B1, cutting D+'s pull-up: the host sees the device go (SE0:
/// a bus reset on the device's side) and gets no more reports; Fn+U lowers
/// it and the host enumerates it again.
#[test]
fn wireless_mode_lifts_b1_and_the_host_lets_the_device_go() {
    let mut t = T::boot_and_enumerate();
    t.b.radio.borrow_mut().bonded[akko_sim::radio::DEVS_2G4] = true;
    t.fn_hold(DONGLE, 0.03);
    t.log_should_show("device detached", 0.5);
    assert!(t.pin_high(GPIOB_ODR, USB_POWER));
    t.b.run_for(0.4);
    t.tap(KEY_A.0, KEY_A.1);
    t.part_should_show(Part::Radio, "^<- a1 00 00 04", 1.0);
    t.hid_not_since(1, "00 00 04");
    t.fn_hold(USB, 0.03);
    t.log_should_show("configured", 2.0);
    t.tap(KEY_A.0, KEY_A.1);
    t.hid_should_show(1, "00 00 04 00 00 00 00 00", 1.0);
    assert_eq!(t.b.host_errors(), Vec::<String>::new());
}

/// The charger pulls A15 low while the cable is in and the battery isn't
/// full; the firmware tells the module (A6 64 charging, 66 done, 65 not
/// charging) and the module reports the level the charger has reached.
#[test]
fn the_charger_drives_a15_and_the_module_hears_of_it() {
    let mut t = T::boot_and_enumerate();
    t.b.run_for(0.5);
    t.b.plug(false);
    t.b.charger.level = 50.0;
    t.part_should_show(Part::Radio, "^<- a6 65 0b", 2.0);
    t.part_should_show(Part::Radio, "^-> 5c 32 8e", 4.0);
    assert!(t.b.chip.gpio.pin_level(CHARGING));
    t.b.plug(true);
    t.b.run_for(0.01);
    assert!(!t.b.chip.gpio.pin_level(CHARGING), "charging");
    t.part_should_show(Part::Radio, "^<- a6 64 0a", 2.0);
    assert_eq!(t.b.radio.borrow().charge, Some(0x64));
    t.b.charger.level = 100.0;
    t.part_should_show(Part::Radio, "^<- a6 66 0c", 2.0);
    assert_eq!(t.b.radio.borrow().charge, Some(0x66));
    assert!(t.b.chip.gpio.pin_level(CHARGING));
}

/// Each contact bounces three times, 100 µs apart, before it settles: one
/// detent is still one volume step.
#[test]
fn a_bouncing_knob_detent_is_one_step() {
    let mut t = T::boot_and_enumerate();
    t.knob_step_bouncing(true, 0.002, 3, 0.0001);
    t.b.run_for(0.3);
    assert_eq!(reports(&t, 2, "03 e9 00"), 1);
    assert!(t.b.knob.at_detent());
}

/// Fn+L, the lighting off: A8 cuts the LEDs' supply and they're dark,
/// whatever was last sent to them.
#[test]
fn the_lighting_off_powers_the_leds_down() {
    let mut t = T::boot_and_enumerate();
    t.b.run_for(2.5);
    assert!(t.led_colours().iter().any(|&c| c != 0));
    t.fn_tap_and_wait(RGB_TOG, 1);
    assert!(!t.led_power());
    assert!(t.led_colours().iter().all(|&c| c == 0));
}
