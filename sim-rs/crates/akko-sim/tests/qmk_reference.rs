//! ../../sim/tests/qmk_reference.robot: the known-good QMK build (flashed and
//! in daily use) run in the simulator. These check the simulator's models
//! against real firmware that works on the board: if one fails after a model
//! change, the model is wrong, not QMK. The image is QMK's ELF, whose flash
//! bytes run.sh checks against the .bin.

mod common;

use akko_sim::usb_host::cmd;
use akko_sim::{Image, Part};
use common::*;
use regex::Regex;
use wb32_sim::diag::Diag;

fn golden(name: &str) -> String {
    let path = format!("{}/../../../sim/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn boot_qmk() -> T {
    T::start(Image::Qmk)
}

/// QMK past what the board model can't run (../../sim/qmk-hooks.resc):
/// spi_flash_start returns false at once, so every flash operation fails
/// fast and QMK goes on with default settings to USB.
fn boot_qmk_with_hooks() -> T {
    let mut t = boot_qmk();
    let addr = Image::Qmk
        .exact_symbol("spi_flash_start")
        .expect("spi_flash_start");
    t.b.cpu.stubs.insert(addr, 0);
    t
}

/// The register writes over `secs` that match `pattern`, as trace.sh lists
/// them; the SPI flash's commands ("FLASH ...") after them.
fn trace(mut t: T, secs: f64, pattern: &str) -> Vec<String> {
    let re = Regex::new(pattern).unwrap();
    t.b.chip.writes = Some(Vec::new());
    t.b.run_for(secs);
    let mut lines: Vec<String> =
        t.b.chip
            .writes
            .take()
            .unwrap()
            .iter()
            .map(|w| w.to_string())
            .filter(|l| re.is_match(l))
            .collect();
    lines.extend(
        t.b.log(Part::Nor)
            .iter()
            .map(|l| format!("FLASH {l}"))
            .filter(|l| re.is_match(l)),
    );
    lines
}

#[test]
fn qmk_clock_setup_respects_the_anctl_key() {
    let mut t = boot_qmk();
    t.b.run_for(0.5);
    let locked: Vec<Diag> =
        t.b.chip
            .take_diags()
            .into_iter()
            .filter(|d| matches!(d, Diag::Locked { .. }))
            .collect();
    assert!(locked.is_empty(), "{locked:?}");
}

#[test]
fn qmk_clock_setup_matches_the_golden_trace() {
    // Renode's QMK stops at the end of the golden trace, stuck on the SPI
    // flash its RETTOBASE leak breaks (qmk_spi_flash_bring_up_...); ours
    // goes on to USB's clocks (qmk_usb_init.txt) at the same point.
    let ours = trace(boot_qmk(), 0.5, r"^(PWR|ANCTL|RCC)");
    let golden = golden("qmk_sysctl_writes.txt");
    let golden: Vec<&str> = golden.trim().lines().collect();
    assert_eq!(&ours[..golden.len()], &golden[..]);
    assert_eq!(
        &ours[golden.len()..golden.len() + 2],
        ["RCC+0x040 = 0x00000100", "RCC+0x040 = 0x00000102"]
    );
}

#[test]
fn qmk_clock_init_matches_its_golden_trace() {
    // The prefix our rcc::init reproduces (m2_hal.rs), cache write included.
    let golden = golden("qmk_clock_init.txt");
    let golden: Vec<&str> = golden.trim().lines().collect();
    let ours = trace(boot_qmk(), 0.5, r"^(PWR|ANCTL|RCC|CACHE)");
    assert_eq!(&ours[..golden.len().min(ours.len())], &golden[..]);
}

#[test]
fn qmk_reaches_the_chibios_scheduler() {
    let mut t = boot_qmk();
    t.b.run_for(0.5);
    let pc = t.b.pc();
    assert!(t.b.in_flash(), "PC 0x{pc:08x}");
    assert!(t.b.cpu.diags.is_empty(), "{:?}", t.b.cpu.diags);
}

#[test]
fn qmk_spi_flash_bring_up_matches_its_golden_trace() {
    // Without the hooks: QSPI setup (BAUDR 0x20 = PCLK1/32, SER, CR0 8-bit
    // mode 0, the FIFO depth probe), chip select on C12 (GPIOC BSRR) and the
    // first flash commands on a blank chip. The golden trace holds up to
    // where Renode goes wrong: in SysTick nested over the QSPI interrupt
    // its ICSR reads RETTOBASE (BOARD_FACTS.md), ChibiOS's epilogue leaks
    // a frame on the idle thread's stack each time, and by the end of the
    // first READ the stack has grown over spi_master.c's spiStarted. So
    // spi_stop skips raising CS, and the next command's bytes count as the
    // READ's (n=2050). Ours raises CS where Renode's chip select stays low.
    let ours = trace(boot_qmk(), 1.5, r"^(QSPI|FLASH|GPIOC\+0x018)");
    let golden = golden("qmk_qspi_init.txt");
    let (flash, writes): (Vec<&str>, Vec<&str>) =
        golden.trim().lines().partition(|l| l.starts_with("FLASH "));
    const CS_LOW: &str = "GPIOC+0x018 = 0x10000000";
    const CS_HIGH: &str = "GPIOC+0x018 = 0x00001000";
    let ours_writes: Vec<&str> = ours
        .iter()
        .map(String::as_str)
        .filter(|l| !l.starts_with("FLASH "))
        .collect();
    // Renode's READ: its chip select low, and the next low with no high
    // between.
    let cs: Vec<(usize, &&str)> = writes
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("GPIOC"))
        .collect();
    let pair = cs
        .windows(2)
        .find(|w| *w[0].1 == CS_LOW && *w[1].1 == CS_LOW)
        .expect("Renode's missing CS high");
    let (read, next) = (pair[0].0, pair[1].0);
    let same = ours_writes
        .iter()
        .zip(&writes)
        .take_while(|(a, b)| a == b)
        .count();
    assert!(
        (read..next).contains(&same),
        "ours parts from Renode at write {same}, outside its READ ({read}..{next})"
    );
    assert_eq!(ours_writes[same], CS_HIGH, "spi_stop after the READ");
    let ours_flash: Vec<&str> = ours
        .iter()
        .map(String::as_str)
        .filter(|l| l.starts_with("FLASH "))
        .collect();
    assert_eq!(
        flash[..2],
        ["FLASH READ 0x000000 n=2050", "FLASH READ 0x000800 n=8"]
    );
    assert_eq!(
        ours_flash[..2],
        ["FLASH READ 0x000000 n=2048", "FLASH READ 0x000800 n=8"],
        "the READ is the 2048 bytes QMK asks for"
    );
}

#[test]
fn qmk_usb_bring_up_matches_its_golden_trace() {
    // m3_usb.rs checks our image against the same file.
    let ours = trace(
        boot_qmk_with_hooks(),
        1.0,
        r"^(RCC\+0x(014|034|040|06c|07c|088)|SFM)",
    );
    assert_eq!(ours.join("\n"), golden("qmk_usb_init.txt").trim());
}

#[test]
fn qmk_enumerates_on_the_usb_model() {
    // The model's control-transfer and FADDR timing against ChibiOS's
    // driver, which sets the address early (before the status stage).
    let mut t = boot_qmk_with_hooks();
    t.log_should_show("SET_ADDRESS(5) -> ok", 1.0);
    t.log_should_show("device 342d:e4f6, EP0 64 bytes", 1.0);
    // Keyboard EP1 IN, VIA raw HID EP2 IN and OUT, shared EP3 IN.
    t.log_should_show(
        "configured: if 0 class 03 eps [81], if 1 class 03 eps [82 02], if 2 class 03 eps [83]",
        1.0,
    );
    assert_eq!(t.b.host_errors(), Vec::<String>::new());
}

#[test]
fn qmk_accepts_the_keyboard_led_report() {
    let mut t = boot_qmk_with_hooks();
    t.b.run_for(1.0);
    t.b.host.command(cmd::HID_LEDS | 0x02 << 8);
    t.log_should_show("SET_REPORT(output) -> ok", 0.05);
    assert_eq!(t.b.host_errors(), Vec::<String>::new());
}
