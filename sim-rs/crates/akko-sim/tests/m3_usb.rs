//! Renode's sim/tests/m3_usb.robot: USB through musb, with the m3-usb image
//! (CDC-ACM log and commands plus an HID boot keyboard) on the scripted host.

mod common;

use akko_sim::usb_host::{CONFIGURED, DETACHED, ENUMERATING, SUSPENDED, UsbEvent, cmd};
use akko_sim::{Board, Image};
use regex::Regex;
use wb32_sim::Diag;

const GPIOB_ODR: u32 = 0x4000_0414;
const GPIOC_ODR: u32 = 0x4000_0814;
const WIN_LOCK_LED: u32 = 0x01;
const CAPS_LED: u32 = 0x20;

/// A booted m3-usb and the place its log has been read up to.
struct T {
    b: Board,
    cursor: usize,
}

impl T {
    fn boot() -> T {
        T {
            b: Board::boot(Image::M3Usb).unwrap(),
            cursor: 0,
        }
    }

    fn boot_and_enumerate() -> T {
        let mut t = T::boot();
        // Attached at ~158 ms (attaches_50_ms_after_start), configured
        // ~145 ms later.
        t.b.run_for(0.4);
        assert_eq!(
            t.b.host.state(),
            CONFIGURED,
            "not configured after 0.4 s: {:?}",
            t.b.host_errors()
        );
        t
    }

    fn host(&mut self, c: u32) {
        self.b.host.command(c);
    }

    /// Types `text` and CR into the CDC port, one OUT packet per byte.
    fn send_line(&mut self, text: &str) {
        for c in text.bytes() {
            self.host((c as u32) << 8 | cmd::CDC_BYTE);
        }
        self.host(0x0D02);
    }

    fn wait(&mut self, secs: f64, what: &str, pred: impl FnMut(&UsbEvent) -> bool) -> UsbEvent {
        match self.b.wait_for(&mut self.cursor, secs, pred) {
            Some(e) => e,
            None => {
                let tail: Vec<String> = self
                    .b
                    .host
                    .events()
                    .iter()
                    .rev()
                    .take(12)
                    .map(|e| e.to_string())
                    .collect();
                panic!("no {what} within {secs} s; last events, newest first: {tail:#?}")
            }
        }
    }

    fn cdc_should_show(&mut self, pattern: &str) {
        let re = Regex::new(pattern).unwrap();
        self.wait(
            2.0,
            pattern,
            |e| matches!(e, UsbEvent::Cdc(l) if re.is_match(l)),
        );
    }

    fn log_should_show(&mut self, line: &str) {
        self.wait(2.0, line, |e| matches!(e, UsbEvent::Log(l) if l == line));
    }

    fn led_on(&mut self, odr: u32, bit: u32) -> bool {
        self.b.word(odr) & bit == 0
    }

    fn no_host_errors(&mut self) {
        assert_eq!(self.b.host_errors(), Vec::<String>::new());
        let diags = self.b.chip.take_diags();
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d, Diag::Locked { .. } | Diag::Unmapped { .. })),
            "{diags:?}"
        );
    }
}

#[test]
fn usb_bring_up_matches_qmk() {
    // Clocks, reset, FIFO clock, PHY and the 48 MHz prescaler, as ChibiOS's wb32_usb_init.
    let golden = common::golden("qmk_usb_init.txt");
    let re = Regex::new(r"^(RCC\+0x(014|034|040|06c|07c|088)|SFM)").unwrap();
    let mut t = T::boot();
    t.b.chip.writes = Some(Vec::new());
    t.b.run_for(0.3);
    let ours: Vec<String> =
        t.b.chip
            .writes
            .iter()
            .flatten()
            .map(|w| w.to_string())
            .filter(|l| re.is_match(l))
            .collect();
    assert_eq!(ours.join("\n"), golden.trim());
}

#[test]
fn enumerates_as_a_cdc_acm_and_hid_composite_device() {
    let mut t = T::boot_and_enumerate();
    t.log_should_show("device 1209:0001, EP0 64 bytes");
    t.log_should_show("string 2 = '5075B Plus M3 USB test'");
    // IN and OUT of one number are separate FIFOs. musb gives each endpoint
    // a free number first and pairs different types only once none is left:
    // the keyboard's IN shares EP2 with CDC data OUT, its OUT shares EP1.
    t.log_should_show(
        "configured: if 0 class 02 eps [81], if 1 class 0a eps [02 83], if 2 class 03 eps [82 01]",
    );
    t.no_host_errors();
}

#[test]
fn attaches_50_ms_after_start() {
    // The bus is held disconnected for at least 50 ms first: a busy-wait,
    // asm::delay(50 ms of cycles), whose rounds take 3 cycles (TRM Table
    // 18-1, and the board's rate, BOARD_FACTS.md), so 150 ms, attaching at
    // ~158 ms with the start-up.
    let mut t = T::boot();
    t.b.run_for(0.149);
    assert_eq!(t.b.host.state(), DETACHED);
    t.b.run_for(0.02);
    assert_eq!(t.b.host.state(), ENUMERATING);
}

#[test]
fn log_starts_with_the_boot_banner() {
    let mut t = T::boot_and_enumerate();
    t.host(cmd::OPEN_CDC);
    t.cdc_should_show(r"\[\s+0\] m3-usb: boot");
    t.cdc_should_show(r"\[\s+0\] no panic record from the previous run");
    t.cdc_should_show(r"\[\s+\d+\] usb: address 5");
    t.cdc_should_show(r"\[\s+\d+\] usb: configured");
    t.cdc_should_show(r"\[\s+\d+\] cdc: port opened");
    t.no_host_errors();
}

#[test]
fn info_command_answers() {
    let mut t = T::boot_and_enumerate();
    t.host(cmd::OPEN_CDC);
    t.send_line("info");
    t.b.run_for(0.05);
    t.cdc_should_show(r"m3-usb: up \d+ ms, configured true, suspended false");
    t.send_line("bogus");
    t.b.run_for(0.05);
    t.cdc_should_show(r#"unknown command "bogus" \(info, type, panic, dfu\)"#);
    t.no_host_errors();
}

#[test]
fn type_command_types_m3_ok_and_enter() {
    let mut t = T::boot_and_enumerate();
    t.send_line("type");
    t.b.run_for(0.1);
    // The log line comes before the host has read the last key-up, so check
    // it first without moving past the HID reports.
    let mut cdc = t.cursor;
    let re = Regex::new(r"hid: typed 6 keys").unwrap();
    assert!(
        t.b.wait_for(
            &mut cdc,
            2.0,
            |e| matches!(e, UsbEvent::Cdc(l) if re.is_match(l))
        )
        .is_some()
    );
    for key in [0x10, 0x20, 0x2c, 0x12, 0x0e, 0x28] {
        let down = [0, 0, key, 0, 0, 0, 0, 0];
        t.wait(
            1.0,
            "key down",
            |e| matches!(e, UsbEvent::Hid { ep: 2, data } if data == &down),
        );
        t.wait(
            1.0,
            "key up",
            |e| matches!(e, UsbEvent::Hid { ep: 2, data } if data == &[0; 8]),
        );
    }
    t.no_host_errors();
}

#[test]
fn caps_lock_led_shows_the_time_base_until_the_host_takes_over() {
    let mut t = T::boot();
    t.b.run_for(0.01);
    assert!(
        t.led_on(GPIOC_ODR, CAPS_LED),
        "Caps Lock LED is off: the time base isn't moving"
    );
    t.b.run_for(0.3);
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.host(0x0003);
    t.b.run_for(0.02);
    assert!(!t.led_on(GPIOC_ODR, CAPS_LED));
    t.cdc_should_show(r"time: \d+ us -> \d+ us across 1 ms of CPU cycles");
}

#[test]
fn host_caps_lock_drives_the_caps_lock_led() {
    let mut t = T::boot_and_enumerate();
    t.host(0x0203);
    t.b.run_for(0.02);
    assert!(t.led_on(GPIOC_ODR, CAPS_LED));
    t.host(0x0003);
    t.b.run_for(0.02);
    assert!(!t.led_on(GPIOC_ODR, CAPS_LED));
    t.no_host_errors();
}

#[test]
fn suspend_and_resume_are_reported_in_order() {
    let mut t = T::boot_and_enumerate();
    assert!(
        t.led_on(GPIOB_ODR, WIN_LOCK_LED),
        "Win Lock LED is off while configured"
    );
    t.host(cmd::SUSPEND);
    t.b.run_for(0.02);
    assert_eq!(t.b.host.state(), SUSPENDED);
    assert!(
        !t.led_on(GPIOB_ODR, WIN_LOCK_LED),
        "Win Lock LED is on while suspended"
    );
    t.host(cmd::RESUME);
    t.b.run_for(0.05);
    assert!(
        t.led_on(GPIOB_ODR, WIN_LOCK_LED),
        "Win Lock LED is off after resume"
    );
    t.host(cmd::OPEN_CDC);
    t.b.run_for(0.02);
    t.cdc_should_show(r"\[\s+\d+\] usb: suspended");
    t.cdc_should_show(r"\[\s+\d+\] usb: resumed");
    t.send_line("info");
    t.b.run_for(0.05);
    t.cdc_should_show("configured true, suspended false");
    t.no_host_errors();
}

#[test]
fn suspend_works_after_every_bus_reset() {
    // A bus reset clears POWER.SUSEN and INTRUSBE.SUSIE; the driver sets them again.
    let mut t = T::boot_and_enumerate();
    t.host(cmd::REENUMERATE);
    t.b.run_for(0.3);
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.host(cmd::SUSPEND);
    t.b.run_for(0.02);
    assert_eq!(t.b.host.state(), SUSPENDED);
    assert!(
        !t.b.host
            .events()
            .iter()
            .any(|e| e.to_string().contains("did not detect suspend"))
    );
    t.no_host_errors();
}

#[test]
fn dfu_command_enters_dfu() {
    let mut t = T::boot_and_enumerate();
    t.send_line("dfu");
    t.b.run_for(0.01);
    assert!(t.b.in_rom(), "PC 0x{:08x}", t.b.pc());
}

#[test]
fn esc_enters_dfu_while_configured() {
    let mut t = T::boot_and_enumerate();
    t.b.key(0, 0, true);
    t.b.run_for(0.002);
    assert!(t.b.in_rom(), "PC 0x{:08x}", t.b.pc());
}

/// boot-guard's policy: the first panic resets into the firmware with its
/// message kept; another before the firmware calls mark_stable (m3-usb
/// never does) enters DFU.
#[test]
fn a_panic_resets_into_the_firmware_a_second_one_enters_dfu() {
    let mut t = T::boot_and_enumerate();
    let previous = r"previous run panicked: firmware/src/bin/m3-usb.rs:\d+:\d+: requested over USB";
    t.send_line("panic");
    t.b.run_for(0.01);
    assert!(t.b.in_flash(), "PC 0x{:08x}", t.b.pc());
    t.b.run_for(0.4);
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.host(cmd::OPEN_CDC);
    t.cdc_should_show(previous);
    t.send_line("panic");
    t.b.run_for(0.01);
    assert!(t.b.in_rom(), "PC 0x{:08x}", t.b.pc());
    // What wb32-dfu-updater_cli -R does: a system reset, RAM kept.
    t.b.system_reset();
    t.b.run_for(0.4);
    assert_eq!(t.b.host.state(), CONFIGURED);
    t.host(cmd::OPEN_CDC);
    t.cdc_should_show(previous);
    // Reported once only: the boot after that finds no record. And DFU
    // cleared the count: the next panic resets into the firmware again.
    t.b.system_reset();
    t.b.run_for(0.4);
    t.host(cmd::OPEN_CDC);
    t.b.run_for(0.02);
    t.cdc_should_show(r"\[\s+0\] no panic record from the previous run");
    t.send_line("panic");
    t.b.run_for(0.01);
    assert!(t.b.in_flash(), "PC 0x{:08x}", t.b.pc());
}

#[test]
fn without_a_host_it_returns_to_dfu_after_30_s() {
    // The 30 s count from when the firmware starts them: after the clocks
    // are up (HSE and the PLL, ~2.2 ms) and USB's enable has blocked for
    // its 50 ms of D+ low (asm::delay cycles: 150 ms, 3 cycles a round).
    let mut t = T::boot();
    t.host(cmd::UNPLUG);
    t.b.run_for(29.9);
    assert!(t.b.in_flash(), "PC 0x{:08x}", t.b.pc());
    assert!(t.b.run_until(0.3, |b| b.in_rom()), "PC 0x{:08x}", t.b.pc());
    let s = t.b.cpu.now as f64 / cortex_m3_emu::TICK_HZ as f64;
    assert!((30.05..30.2).contains(&s), "DFU at {s} s");
}

#[test]
fn configured_stays_up_past_30_s() {
    let mut t = T::boot_and_enumerate();
    t.b.run_for(31.0);
    assert!(t.b.in_flash(), "PC 0x{:08x}", t.b.pc());
    assert_eq!(t.b.host.state(), CONFIGURED);
}
