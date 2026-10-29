//! ../../sim/tests/m2_hal.robot: the HAL core (clocks, SysTick time base,
//! GPIO, EXTI) on embassy, through the m2-blink image.

use akko_sim::{Board, Image};
use wb32_sim::Diag;

const GPIOC_ODR: u32 = 0x4000_0814;
const CAPS_LED: u32 = 0x20;
const SYST_CSR: u32 = 0xE000_E010;
const SYST_RVR: u32 = 0xE000_E014;
const SYST_CVR: u32 = 0xE000_E018;
/// boot-guard's Esc check comes before main and is covered by boot_guard.rs.
const BOOT_GUARD_WRITES: usize = 6;

fn boot() -> Board {
    Board::boot(Image::M2Blink).unwrap()
}

fn caps_led_on(b: &mut Board) -> bool {
    b.word(GPIOC_ODR) & CAPS_LED == 0 // active low
}

/// The time driver's time in ms: DRIVER's first fields (the ticks, us, up
/// to the current period, u64, and the cycles past that tick) plus how far
/// SysTick is into that period (its reload minus its count).
fn milliseconds(b: &mut Board) -> u64 {
    let addr = Image::M2Blink
        .symbol("11time_driver6DRIVER")
        .expect("wb32_hal::time_driver::DRIVER");
    let (lo, hi, frac) = (b.word(addr), b.word(addr + 4), b.word(addr + 8));
    let (rvr, cvr) = (b.word(SYST_RVR), b.word(SYST_CVR));
    (((hi as u64) << 32 | lo as u64) + (frac + rvr - cvr) as u64 / 96) / 1000
}

#[test]
fn clock_setup_matches_qmk() {
    let golden = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../sim/golden/qmk_clock_init.txt"
    ))
    .unwrap();
    let golden: Vec<&str> = golden.trim().lines().collect();
    let mut b = boot();
    b.chip.writes = Some(Vec::new());
    b.run_for(0.05);
    let ours: Vec<String> = b
        .chip
        .writes
        .iter()
        .flatten()
        .filter(|w| matches!(w.block, "PWR" | "ANCTL" | "RCC" | "CACHE"))
        .map(|w| w.to_string())
        .skip(BOOT_GUARD_WRITES)
        .take(golden.len())
        .collect();
    assert_eq!(ours, golden);
}

#[test]
fn only_modelled_peripherals_are_touched() {
    let mut b = boot();
    b.run_for(1.0);
    let diags = b.chip.take_diags();
    assert!(diags.is_empty(), "{:?}", &diags[..diags.len().min(8)]);
    assert!(!diags.iter().any(|d| matches!(d, Diag::Locked { .. })));
}

#[test]
fn systick_runs_tickless() {
    // Core clock, interrupt on; the reload is set to the next alarm (the
    // blink's 500 ms is past the counter's reach: its longest, 2^24 cycles).
    let mut b = boot();
    b.run_for(0.1);
    assert_eq!(b.word(SYST_CSR) & 7, 7);
    assert_eq!(b.word(SYST_RVR), (1 << 24) - 1);
}

/// rcc::init: HSE (12 MHz), the PLL at HSE / 2 x 16, MAINCLK from it, AHB
/// and both APBs undivided (the clock tree from the RM, sysctl.rs).
#[test]
fn the_clocks_are_96_mhz_after_start_up() {
    let mut b = boot();
    b.run_for(0.01);
    let c = b.chip.sysctl.clocks();
    assert_eq!(
        (c.mainclk, c.hclk, c.pclk1, c.pclk2),
        (96_000_000, 96_000_000, 96_000_000, 96_000_000)
    );
}

#[test]
fn time_base_counts_milliseconds() {
    // From after start-up: the time base starts once the clocks are up
    // (rcc::init waits ~2.2 ms for HSE and the PLL to lock).
    let mut b = boot();
    b.run_for(0.1);
    let start = milliseconds(&mut b);
    b.run_for(1.0);
    let ms = milliseconds(&mut b) - start;
    assert!((999..=1001).contains(&ms), "time base {ms} ms over 1 s");
}

#[test]
fn caps_led_blinks_for_10_s_then_returns_to_dfu() {
    // Toggles at t = 0, 0.5, ..., 9.5 s; DFU at 10 s. Each edge is checked
    // within +-10 ms, and the LED must hold still in between.
    let mut b = boot();
    b.run_for(0.49);
    for i in 1..20 {
        let before = caps_led_on(&mut b);
        b.run_for(0.02);
        let after = caps_led_on(&mut b);
        assert_ne!(before, after, "no toggle at {} s", i as f64 * 0.5);
        b.run_for(0.24);
        assert_eq!(
            caps_led_on(&mut b),
            after,
            "extra toggle before {} s",
            i as f64 * 0.5 + 0.25
        );
        b.run_for(0.24);
    }
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
    b.run_for(0.02);
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
}

#[test]
fn esc_returns_to_dfu_at_once() {
    let mut b = boot();
    b.run_for(2.2);
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
    b.key(0, 0, true);
    b.run_for(0.002);
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
}

#[test]
fn other_keys_on_the_esc_row_do_not() {
    let mut b = boot();
    b.run_for(1.0);
    // F1 shares row A0, but its column (C1) isn't driven, so A0 stays high.
    b.key(0, 1, true);
    b.run_for(1.0);
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
}
