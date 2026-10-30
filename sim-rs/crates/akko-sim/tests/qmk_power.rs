//! ../../sim/compare/qmk_power.robot: m7_power's CPU numbers for QMK, for
//! the README's comparison. QMK's main loop never sleeps, so 0.2 s is
//! enough. spi_flash_start is stubbed as there (qmk_reference.rs's hooks).
//! Its key reports go to the radio module here (its settings can't be read),
//! so latency isn't measured: the README takes it from the source. The
//! image is qmk-via; any QMK build busy-loops the same way.

mod common;

use akko_sim::Image;
use common::*;

const RGB_TOG: (u8, u8) = (3, 9);
const KEY_A: (u8, u8) = (3, 1);
// QMK never sleeps: awake every cycle of its 96 MHz HCLK. With the TRM's
// cycle costs (hardware-gaps step 4) that's ~60 M instructions a second
// (96 M at one a cycle in step 3; Renode's flat 100 M/s before 2026-10-29).
// Its SysTick runs at 100 kHz (Renode's at 100.1 kHz, its RVR period:
// BOARD_FACTS.md).
const MIN_CYCLES: f64 = 95_900_000.0;
const SYSTICK_HZ: f64 = 100_000.0;

fn boot() -> T {
    let mut t = T::start(Image::Qmk);
    let addr = Image::Qmk.exact_symbol("spi_flash_start").unwrap();
    t.b.cpu.stubs.insert(addr, 0);
    t.log_should_show("configured", 6.0);
    t.b.run_for(0.5);
    t
}

/// Runs `secs`; core clocks awake per second (printed with the
/// instructions, sleeps and SysTick's rate, which it checks).
fn measure(t: &mut T, label: &str, secs: f64) -> f64 {
    let systick = Image::Qmk.exact_symbol("SysTick_Handler").unwrap();
    t.b.chip.pc_hits = Some([(systick, 0)].into());
    let (insns, cycles, sleeps) = (t.b.cpu.instructions, t.b.cpu.cycles, t.b.cpu.sleeps);
    t.b.run_for(secs);
    let rate = (t.b.cpu.instructions - insns) as f64 / secs;
    let awake = (t.b.cpu.cycles - cycles) as f64 / secs;
    let ticks = t.b.chip.pc_hits.as_ref().unwrap()[&systick] as f64 / secs;
    println!(
        "QMKPOWER {label}: {:.2} M insns/s, {:.2} M cycles/s; {:.0} sleeps/s, SysTick {ticks:.0}",
        rate / 1e6,
        awake / 1e6,
        (t.b.cpu.sleeps - sleeps) as f64 / secs,
    );
    assert!((ticks - SYSTICK_HZ).abs() <= 1.0 / secs, "{ticks}");
    t.no_errors();
    awake
}

#[test]
fn qmk_lights_on() {
    let mut t = boot();
    assert!(measure(&mut t, "lights on, idle", 0.2) >= MIN_CYCLES);
}

#[test]
fn qmk_lights_off() {
    let mut t = boot();
    t.fn_tap(RGB_TOG, 1);
    t.b.run_for(0.2);
    assert!(measure(&mut t, "lights off, idle", 0.2) >= MIN_CYCLES);
}

#[test]
fn qmk_key_held() {
    let mut t = boot();
    t.key(KEY_A.0, KEY_A.1, true);
    t.b.run_for(0.1);
    assert!(measure(&mut t, "key held", 0.2) >= MIN_CYCLES);
}
