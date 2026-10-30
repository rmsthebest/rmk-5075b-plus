//! ../../sim/tests/m7_power.robot: what the firmware costs while idle, and
//! how soon a key reaches USB. A sleeping core jumps to its next event, so
//! the instructions executed per second of emulated time stand for the time
//! the CPU is awake; the chip counts how often each interrupt handler runs
//! (Renode's `cpu AddHook`s, powercount.py) and the core how often it goes
//! back to sleep. The numbers are printed (`--nocapture`) and held under the
//! robot file's bounds, which are the measured values with some headroom.
//! Latency is the time from a key's press to the device arming the
//! keyboard's IN endpoint (usb.py's 0xF14), in the host's 100 us steps; the
//! host's 1 ms polling comes on top.

mod common;

use akko_sim::Part;
use common::*;
use std::collections::{BTreeMap, HashMap};

const KEYBOARD_EP: u8 = 1;
const RGB_TOG: (u8, u8) = (3, 9);
const KEY_A: (u8, u8) = (3, 1);
const DONGLE: (u8, u8) = (2, 6);
const SEL_DONGLE: &str = "a6 30 d6";
const ALL_UP: &str = "a1 00 00 00 00 00 00 00 00 a1";
const HANDLERS: [&str; 12] = [
    "SysTick",
    "TIM2",
    "EXTI0",
    "EXTI1",
    "EXTI2",
    "EXTI3",
    "EXTI4",
    "EXTI9_5",
    "EXTI15_10",
    "USB",
    "UART3",
    "SPIM2",
];
// Per emulated second. Until 2026-10-29 these were the robot file's bounds
// (its header has the history); since the core runs at HCLK (96 MHz, not
// Renode's flat 100 MIPS; hardware-gaps step 3) they're kept here:
//   step 3: on 6.00 M, 74 sleeps; off 0.22 M, 76; asleep 0.07 M, 18; key held
//   33.4 M; press 5 steps (all 10), release 43-56 (mean 48.5).
//   2026-10-30, presses over a whole frame (160): press 5, 12 on a frame
//   send (mean 5.2); release 57 worst (mean 48.5).
//   step 4 (TRM cycle costs, CPI ~1.6 awake): instructions about the same
//   (on 5.90 M, off 0.21 M, asleep 0.07 M, key held 32.7 M) but the time
//   awake grew with them; press 6-7 steps, 17 on a frame send (mean 6.7);
//   release 63 worst (mean 48.7).
const MAX_INSNS_LIGHTS_ON: f64 = 6_800_000.0;
const MAX_WAKES_LIGHTS_ON: f64 = 90.0;
const MAX_INSNS_LIGHTS_OFF: f64 = 300_000.0;
const MAX_WAKES_LIGHTS_OFF: f64 = 90.0;
const MAX_INSNS_ASLEEP: f64 = 100_000.0;
const MAX_WAKES_ASLEEP: f64 = 25.0;
const MAX_INSNS_KEY_HELD: f64 = 37_000_000.0;
// Press (release) to IN endpoint armed, in 100 us steps: worst over the
// presses in `press_to_report_latency`. Releases wait out the 5 ms debounce.
// A press is 6-7 steps (3-4 at Renode's 100 MIPS, 5 at one cycle an
// instruction), unless it lands on a WS2812 frame send (lights on): the
// scan waits for it, 17 steps with the TRM's cycles. The presses sweep a
// whole frame, so the worst always includes that case.
const MAX_PRESS_WORST: u32 = 18;
const MAX_RELEASE_WORST: u32 = 66;

/// From a blank flash, configured, then past the USB key's 2 s of white and
/// the first settings save.
fn boot() -> T {
    let mut t = T::boot();
    t.log_should_show("configured", 4.0);
    t.b.run_for(4.0);
    t
}

/// Counts every handler in HANDLERS from now on.
fn count_hooks(t: &mut T) {
    let hits = HANDLERS
        .iter()
        .map(|name| {
            let addr = t
                .image
                .exact_symbol(name)
                .unwrap_or_else(|| panic!("no {name}"));
            (addr, 0)
        })
        .collect();
    t.b.chip.pc_hits = Some(hits);
}

struct Counts {
    insns: f64,
    /// Core clocks awake.
    cycles: f64,
    sleeps: f64,
    handlers: BTreeMap<&'static str, f64>,
}

/// Runs `secs` and returns the counts per second.
fn measure(t: &mut T, label: &str, secs: f64) -> Counts {
    let addrs: HashMap<u32, &'static str> = HANDLERS
        .iter()
        .map(|&name| (t.image.exact_symbol(name).unwrap(), name))
        .collect();
    let hits = t.b.chip.pc_hits.as_mut().expect("count_hooks first");
    hits.values_mut().for_each(|n| *n = 0);
    let (insns, cycles, sleeps) = (t.b.cpu.instructions, t.b.cpu.cycles, t.b.cpu.sleeps);
    t.b.run_for(secs);
    let c = Counts {
        insns: (t.b.cpu.instructions - insns) as f64 / secs,
        cycles: (t.b.cpu.cycles - cycles) as f64 / secs,
        sleeps: (t.b.cpu.sleeps - sleeps) as f64 / secs,
        handlers: t
            .b
            .chip
            .pc_hits
            .as_ref()
            .unwrap()
            .iter()
            .filter(|(_, n)| **n != 0)
            .map(|(a, &n)| (addrs[a], n as f64 / secs))
            .collect(),
    };
    let handlers: Vec<String> = c
        .handlers
        .iter()
        .map(|(k, v)| format!("{k} {v:.0}"))
        .collect();
    println!(
        "POWER {label}: {:.2} M insns/s, {:.2} M cycles/s, {:.0} sleeps/s; {}",
        c.insns / 1e6,
        c.cycles / 1e6,
        c.sleeps,
        handlers.join(", ")
    );
    c
}

/// Fn+Y, the link up, the cable out (on the battery, as it's used), then
/// the host suspends: asleep.
fn on_the_dongle_asleep(t: &mut T) {
    t.b.radio.borrow_mut().bonded[akko_sim::radio::DEVS_2G4] = true;
    t.fn_tap(DONGLE, 1);
    t.part_should_show(Part::Radio, &format!("^<- {SEL_DONGLE}"), 1.0);
    t.part_should_show(Part::Radio, &format!("^<- {ALL_UP}"), 1.0);
    t.b.plug(false);
    t.b.run_for(2.5);
    t.b.dongle_host(false);
    t.b.run_for(0.5);
}

/// Presses (or releases) `pos`; the steps until the keyboard endpoint is
/// armed with a new report.
fn press_latency(t: &mut T, (row, col): (u8, u8), down: bool) -> u32 {
    let ready = |t: &T| t.b.host.ready_step(KEYBOARD_EP).unwrap_or(u32::MAX);
    let before = ready(t);
    let pressed = t.b.host.steps();
    t.key(row, col, down);
    for _ in 0..100 {
        t.b.run_for(0.0001);
        let now = ready(t);
        if now != before && now != u32::MAX {
            return now - pressed;
        }
    }
    panic!("no report within 10 ms");
}

#[test]
fn idle_power_with_the_lights_on() {
    let mut t = boot();
    count_hooks(&mut t);
    let c = measure(&mut t, "lights on, idle", 1.0);
    assert!(c.insns <= MAX_INSNS_LIGHTS_ON, "{}", c.insns);
    assert!(c.sleeps <= MAX_WAKES_LIGHTS_ON, "{}", c.sleeps);
    t.no_errors();
}

#[test]
fn idle_power_with_the_lights_off() {
    let mut t = boot();
    t.fn_tap(RGB_TOG, 1);
    t.b.run_for(1.0);
    count_hooks(&mut t);
    let c = measure(&mut t, "lights off, idle", 1.0);
    assert!(c.insns <= MAX_INSNS_LIGHTS_OFF, "{}", c.insns);
    assert!(c.sleeps <= MAX_WAKES_LIGHTS_OFF, "{}", c.sleeps);
    t.no_errors();
}

#[test]
fn idle_power_asleep_on_the_dongle() {
    let mut t = boot();
    on_the_dongle_asleep(&mut t);
    count_hooks(&mut t);
    let c = measure(&mut t, "asleep on the dongle", 1.0);
    assert!(c.insns <= MAX_INSNS_ASLEEP, "{}", c.insns);
    assert!(c.sleeps <= MAX_WAKES_ASLEEP, "{}", c.sleeps);
    t.no_errors();
}

#[test]
fn power_with_a_key_held() {
    let mut t = boot();
    t.key(KEY_A.0, KEY_A.1, true);
    t.b.run_for(0.1);
    count_hooks(&mut t);
    let c = measure(&mut t, "key held", 1.0);
    t.key(KEY_A.0, KEY_A.1, false);
    assert!(c.insns <= MAX_INSNS_KEY_HELD, "{}", c.insns);
    t.no_errors();
}

/// Presses and releases after idling (so an event-driven scan is parked when
/// they come) for 50 ms plus 0-16 ms in 100 us steps: the lighting frames run
/// on from the last key, so this lands a press on every phase of a 16 ms frame,
/// its WS2812 send included.
#[test]
fn press_to_report_latency() {
    let mut t = boot();
    let (mut presses, mut releases) = (Vec::new(), Vec::new());
    for phase in 0..160 {
        t.b.run_for(0.05 + phase as f64 * 0.0001);
        presses.push(press_latency(&mut t, KEY_A, true));
        t.b.run_for(0.03);
        releases.push(press_latency(&mut t, KEY_A, false));
    }
    let worst = |v: &[u32]| *v.iter().max().unwrap();
    let mean = |v: &[u32]| v.iter().sum::<u32>() as f64 / v.len() as f64;
    println!(
        "LATENCY (100 us steps) press {presses:?} worst {} mean {:.1}; release {releases:?} worst {} mean {:.1}",
        worst(&presses),
        mean(&presses),
        worst(&releases),
        mean(&releases)
    );
    assert!(worst(&presses) <= MAX_PRESS_WORST);
    assert!(worst(&releases) <= MAX_RELEASE_WORST);
    t.no_errors();
}
