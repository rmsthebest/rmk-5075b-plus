//! ../../sim/tests/boot_guard.robot: boot-guard's escape hatch, through the
//! m1b-stub and m1a-bounce images. "In DFU" means parked in the ROM stub.

use akko_sim::{Board, Image};
use cortex_m3_emu::{Bus, Size};
use wb32_sim::Diag;

const MAGIC_ADDR: u32 = 0x2000_6FFC;
const MAGIC: u32 = 0xDEAD_BEEF;
const ESC: (u8, u8) = (0, 0);

fn boot(image: Image) -> Board {
    Board::boot(image).unwrap()
}

/// Boots with every register write recorded.
fn boot_recording(image: Image) -> Board {
    let mut b = boot(image);
    b.chip.writes = Some(Vec::new());
    b
}

fn writes(b: &Board) -> Vec<String> {
    b.chip
        .writes
        .iter()
        .flatten()
        .map(|w| w.to_string())
        .collect()
}

fn no_locked(b: &mut Board) {
    let locked: Vec<_> = b
        .chip
        .take_diags()
        .into_iter()
        .filter(|d| matches!(d, Diag::Locked { .. }))
        .collect();
    assert!(locked.is_empty(), "{locked:?}");
}

#[test]
fn normal_boot_stays_in_firmware() {
    let mut b = boot(Image::M1bStub);
    b.run_for(0.2);
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
    assert_eq!(b.word(MAGIC_ADDR), 0);
    no_locked(&mut b);
}

#[test]
fn esc_held_at_cold_start_enters_dfu() {
    let mut b = boot(Image::M1bStub);
    b.key(ESC.0, ESC.1, true);
    b.run_for(0.2);
    assert!(b.resets >= 1, "no SYSRESETREQ");
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
    // boot-guard clears the magic word before jumping, so QMK/Rust start clean next time
    assert_eq!(b.word(MAGIC_ADDR), 0);
}

#[test]
fn magic_word_enters_dfu_without_touching_gpio() {
    let mut b = boot_recording(Image::M1bStub);
    b.chip.write(MAGIC_ADDR, Size::Word, MAGIC);
    b.run_for(0.1);
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
    assert_eq!(b.word(MAGIC_ADDR), 0);
    let gpio: Vec<_> = writes(&b)
        .into_iter()
        .filter(|w| w.starts_with("GPIO"))
        .collect();
    assert!(gpio.is_empty(), "{gpio:?}");
}

#[test]
fn esc_check_restores_pin_state() {
    let mut b = boot(Image::M1bStub);
    b.run_for(0.01);
    // A0: input, no pull; C0: input; GPIOA's CFGMSK back to reset. (m1b's main has
    // already made C5 an output for the LED, so only C0's field is checked on GPIOC.)
    assert_eq!(b.word(0x4000_0000) & 3, 0, "GPIOA MODER, A0");
    assert_eq!(b.word(0x4000_000C) & 3, 0, "GPIOA PUPDR, A0");
    assert_eq!(b.word(0x4000_0030), 0, "GPIOA CFGMSK");
    assert_eq!(b.word(0x4000_0800) & 3, 0, "GPIOC MODER, C0");
}

#[test]
fn other_keys_do_not_enter_dfu() {
    let mut b = boot(Image::M1bStub);
    // F1 (row 0, col 1) and LCtrl (row 5, col 0): same row or same column as Esc
    b.key(0, 1, true);
    b.key(5, 0, true);
    b.run_for(0.2);
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
}

#[test]
fn caps_lock_led_blinks() {
    // At the reset clock (MHSI, 8 MHz) each asm::delay(4_000_000) is 4 M
    // rounds of subs/bne: 1 s here at a cycle an instruction, 1.5 s on the
    // board (3 cycles a round, BOARD_FACTS.md).
    let mut b = boot_recording(Image::M1bStub);
    b.run_for(3.5);
    // BSRR on C5: reset half (LED on, active low), then set half (off)
    let w = writes(&b);
    let on = w
        .iter()
        .position(|w| w == "GPIOC+0x018 = 0x00200000")
        .expect("LED on");
    assert!(
        w[on..].iter().any(|w| w == "GPIOC+0x018 = 0x00000020"),
        "LED never off"
    );
}

#[test]
fn m1b_returns_to_dfu_on_its_own() {
    // 30 blinks of two delays each (see caps_lock_led_blinks): a minute
    // here, 90 s on the board.
    let mut b = boot(Image::M1bStub);
    b.run_for(55.0);
    assert!(b.in_flash(), "PC 0x{:08x}", b.pc());
    assert!(b.run_until(40.0, |b| b.in_rom()), "PC 0x{:08x}", b.pc());
    let s = b.cpu.now as f64 / cortex_m3_emu::TICK_HZ as f64;
    println!("m1b back in DFU at {s:.1} s");
}

#[test]
fn hardfault_lands_in_dfu() {
    let mut b = boot(Image::M1bStub);
    b.run_for(0.05);
    // UDF #0 in RAM; with no UsageFault handler enabled it escalates to HardFault
    b.chip.write(0x2000_5000, Size::Half, 0xDE00);
    b.cpu.r[15] = 0x2000_5000;
    b.run_for(0.1);
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
}

#[test]
fn m1a_bounces_straight_into_dfu() {
    let mut b = boot_recording(Image::M1aBounce);
    b.run_for(0.05);
    assert!(b.in_rom(), "PC 0x{:08x}", b.pc());
    assert!(writes(&b).is_empty(), "{:?}", writes(&b));
}
