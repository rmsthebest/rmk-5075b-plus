//! M3 image: USB through musb, as one composite device (VID:PID 1209:0001,
//! the pid.codes test ID, so the host never mixes it up with the QMK build):
//! - CDC-ACM serial port: a log (boot banner, the panic message the previous
//!   run left, bus events) and line commands: `info`, `type`, `panic`, `dfu`.
//! - HID boot keyboard: `type` types "m3 ok" and Enter. The host's Caps Lock
//!   state (output report on the interrupt OUT endpoint) drives the Caps Lock
//!   LED (C5, active low).
//! - The Win Lock LED (B0, active low) is on while the host has the device
//!   configured and the bus isn't suspended.
//!
//! Ways back to DFU: the `dfu` command, Esc (EXTI on row A0, column C0 held
//! low), any panic, or on its own if the host hasn't configured the device
//! 30 s after boot.
//!
//! Time-base check (the first hardware run had embassy-time stuck): at boot,
//! embassy-time's clock is read across a 1 ms wait counted in CPU cycles.
//! The result goes to the log, and Caps Lock is lit if the clock moved, until
//! the host sends its first LED report.

#![no_std]
#![no_main]

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

use boot_guard as _;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pipe::Pipe;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{self, CdcAcmClass};
use embassy_usb::class::hid::{self, HidReaderWriter, HidSubclass, HidBootProtocol, ReportId};
use embassy_usb::control::OutResponse;
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Handler};
use wb32_hal::exti::ExtiInput;
use wb32_hal::gpio::{Level, Output, Pins, Pull};
use wb32_hal::{rcc, time_driver, usb};

/// NVIC priorities (upper 4 bits): the time base above everything else.
const PRIO_TIME: u8 = 0x40;
const PRIO_USB: u8 = 0x60;
const PRIO_EXTI: u8 = 0x80;

/// Back to DFU if the host hasn't configured the device by then.
const CONFIGURE_TIMEOUT: Duration = Duration::from_secs(30);
/// Log output waits this long after the host opens the port.
const ECHO_GRACE: Duration = Duration::from_millis(100);

const VID: u16 = 0x1209;
const PID: u16 = 0x0001;
const PACKET: u16 = 64;

/// Log bytes waiting for the host to open the serial port. When full, new
/// text is dropped (the boot banner, written first, survives).
static LOG: Pipe<CriticalSectionRawMutex, 1024> = Pipe::new();

struct LogWriter;

impl core::fmt::Write for LogWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let mut b = s.as_bytes();
        while !b.is_empty() {
            match LOG.try_write(b) {
                Ok(n) => b = &b[n..],
                Err(_) => return Err(core::fmt::Error),
            }
        }
        Ok(())
    }
}

macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = write!(LogWriter, "[{:>7}] ", Instant::now().as_millis());
        let _ = write!(LogWriter, $($arg)*);
        let _ = LogWriter.write_str("\r\n");
    }};
}

static CONFIGURED: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
/// Wakes the LED task when either flag above changes.
static USB_STATE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Host keyboard LEDs (bit 1 = Caps Lock), from the HID output report.
static HOST_LEDS: Signal<CriticalSectionRawMutex, u8> = Signal::new();

struct DeviceEvents;

impl Handler for DeviceEvents {
    fn enabled(&mut self, enabled: bool) {
        log!("usb: {}", if enabled { "attached" } else { "detached" });
    }
    fn reset(&mut self) {
        CONFIGURED.store(false, Ordering::Relaxed);
        USB_STATE.signal(());
        log!("usb: bus reset");
    }
    fn addressed(&mut self, addr: u8) {
        log!("usb: address {}", addr);
    }
    fn configured(&mut self, configured: bool) {
        CONFIGURED.store(configured, Ordering::Relaxed);
        USB_STATE.signal(());
        log!("usb: {}", if configured { "configured" } else { "unconfigured" });
    }
    fn suspended(&mut self, suspended: bool) {
        SUSPENDED.store(suspended, Ordering::Relaxed);
        USB_STATE.signal(());
        log!("usb: {}", if suspended { "suspended" } else { "resumed" });
    }
}

struct KeyboardRequests;

impl hid::RequestHandler for KeyboardRequests {
    fn set_report(&mut self, id: ReportId, data: &[u8]) -> OutResponse {
        if let (ReportId::Out(0), [leds, ..]) = (id, data) {
            HOST_LEDS.signal(*leds);
        }
        OutResponse::Accepted
    }
}

/// HID 1.11 appendix B.1 boot keyboard: modifiers, reserved, 6 keys in;
/// 5 LED bits out.
#[rustfmt::skip]
const KEYBOARD_REPORT: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x95, 0x05, 0x75, 0x01, 0x91, 0x02,
    0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
    0x05, 0x07, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00,
    0xC0,
];

/// "m3 ok" and Enter, as HID usage IDs.
const TEST_KEYS: &[u8] = &[0x10, 0x20, 0x2C, 0x12, 0x0E, 0x28];

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    rcc::init();
    time_driver::init(PRIO_TIME);
    let p = Pins::take().unwrap();

    let mut panic_buf = [0; boot_guard::PANIC_MSG_MAX];
    log!("m3-usb: boot");
    match boot_guard::take_panic_message(&mut panic_buf) {
        Some(msg) => log!("previous run panicked: {}", msg),
        None => log!("no panic record from the previous run"),
    }

    let mut caps = Output::new(p.PC5, Level::High); // off
    // On until the host's first LED report (sent once it has enumerated us).
    if check_time_base() {
        caps.set_low();
    }
    let mut win_lock = Output::new(p.PB0, Level::High); // off
    // USB power path on (5075b_plus.c drives USB_POWER_EN_PIN low).
    let _usb_power = Output::new(p.PB1, Level::Low);
    let _esc_col = Output::new(p.PC0, Level::Low);
    let mut esc = ExtiInput::new(p.PA0, Pull::Up, PRIO_EXTI);

    let driver = usb::Driver::new(p.PA11, p.PA12, PRIO_USB);
    let mut config = embassy_usb::Config::new(VID, PID);
    config.manufacturer = Some("akko-rmk");
    config.product = Some("5075B Plus M3 USB test");
    config.serial_number = Some("m3");
    config.max_power = 500;
    config.max_packet_size_0 = PACKET as u8;
    config.supports_remote_wakeup = true;

    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 64];
    let mut control_buf = [0; 64];
    let mut cdc_state = cdc_acm::State::new();
    let mut hid_state = hid::State::new();
    let mut events = DeviceEvents;
    let mut requests = KeyboardRequests;

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no Microsoft OS descriptors
        &mut control_buf,
    );
    builder.handler(&mut events);
    let cdc = CdcAcmClass::new(&mut builder, &mut cdc_state, PACKET);
    let keyboard = HidReaderWriter::<_, 1, 8>::new(
        &mut builder,
        &mut hid_state,
        hid::Config {
            report_descriptor: KEYBOARD_REPORT,
            request_handler: Some(&mut requests),
            poll_ms: 1,
            max_packet_size: 8,
            hid_subclass: HidSubclass::Boot,
            hid_boot_protocol: HidBootProtocol::Keyboard,
        },
    );
    let mut device = builder.build();

    let (mut tx, mut rx, control) = cdc.split_with_control();
    let (kb_reader, mut kb_writer) = keyboard.split();
    let type_request: Signal<CriticalSectionRawMutex, ()> = Signal::new();

    let log_out = async {
        let mut buf = [0; PACKET as usize - 1]; // short packets end each host read
        let mut pending = 0;
        loop {
            tx.wait_connection().await;
            // A port opens with echo on, until the client sets it raw: the
            // backlog sent at once would come back as commands.
            Timer::after(ECHO_GRACE).await;
            loop {
                if pending == 0 {
                    pending = LOG.read(&mut buf).await;
                }
                // Kept on failure, so a bus reset doesn't drop it.
                if tx.write_packet(&buf[..pending]).await.is_err() {
                    break;
                }
                pending = 0;
            }
        }
    };

    // Every open and close of the port, so a program reading it besides
    // tools/m3-check.py shows up in the log.
    let port_watch = async {
        let mut dtr = false;
        loop {
            control.control_changed().await;
            if control.dtr() != dtr {
                dtr = control.dtr();
                log!("cdc: port {}", if dtr { "opened" } else { "closed" });
            }
        }
    };

    let commands = async {
        let mut line = heapless::Vec::<u8, 32>::new();
        let mut packet = [0; PACKET as usize];
        loop {
            rx.wait_connection().await;
            loop {
                let n = match rx.read_packet(&mut packet).await {
                    Ok(n) => n,
                    Err(EndpointError::Disabled) => break,
                    Err(EndpointError::BufferOverflow) => continue,
                };
                for &c in &packet[..n] {
                    if c == b'\r' || c == b'\n' {
                        run_command(&line, &type_request);
                        line.clear();
                    } else if line.push(c).is_err() {
                        line.clear();
                    }
                }
            }
        }
    };

    let typing = async {
        loop {
            type_request.wait().await;
            for &key in TEST_KEYS {
                for report in [[0, 0, key, 0, 0, 0, 0, 0], [0; 8]] {
                    if let Err(e) = kb_writer.write(&report).await {
                        log!("hid: write failed: {:?}", e);
                    }
                }
            }
            log!("hid: typed {} keys", TEST_KEYS.len());
        }
    };

    let host_leds = async {
        let mut requests = KeyboardRequests;
        join(kb_reader.run(false, &mut requests), async {
            loop {
                let leds = HOST_LEDS.wait().await;
                caps.set_level(Level::from(leds & 0x02 == 0)); // active low
                log!("hid: host LEDs {:#04x}", leds);
            }
        })
        .await
    };

    let state_led = async {
        loop {
            let on = CONFIGURED.load(Ordering::Relaxed) && !SUSPENDED.load(Ordering::Relaxed);
            win_lock.set_level(Level::from(!on)); // active low
            USB_STATE.wait().await;
        }
    };

    let watchdog = async {
        Timer::after(CONFIGURE_TIMEOUT).await;
        if !CONFIGURED.load(Ordering::Relaxed) {
            boot_guard::request_bootloader();
        }
        core::future::pending::<()>().await
    };

    let usb_side = join(join(join(device.run(), log_out), port_watch), join(commands, typing));
    let app_side = join(join(host_leds, state_led), watchdog);
    match select3(usb_side, app_side, esc.wait_for_falling_edge()).await {
        Either3::First(_) | Either3::Second(_) | Either3::Third(()) => boot_guard::request_bootloader(),
    }
}

/// Reads embassy-time's clock across a 1 ms busy wait; logs and returns
/// whether it moved (expected: about 1000 us; flash wait states make the
/// wait longer on the board).
fn check_time_base() -> bool {
    let before = Instant::now().as_micros();
    cortex_m::asm::delay(rcc::HCLK_HZ / 1000);
    let after = Instant::now().as_micros();
    log!("time: {} us -> {} us across 1 ms of CPU cycles", before, after);
    after != before
}

fn run_command(line: &[u8], type_request: &Signal<CriticalSectionRawMutex, ()>) {
    match line {
        b"" => {}
        b"info" => {
            log!(
                "m3-usb: up {} ms, configured {}, suspended {}",
                Instant::now().as_millis(),
                CONFIGURED.load(Ordering::Relaxed),
                SUSPENDED.load(Ordering::Relaxed)
            );
        }
        b"type" => type_request.signal(()),
        b"panic" => panic!("requested over USB"),
        b"dfu" => boot_guard::request_bootloader(),
        _ => log!("unknown command \"{}\" (info, type, panic, dfu)", line.escape_ascii()),
    }
}
