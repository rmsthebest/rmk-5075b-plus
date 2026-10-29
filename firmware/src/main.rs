//! The keyboard firmware: RMK over USB (M4).
//!
//! Keys (src/scan.rs), the knob (src/knob.rs), the keymap from the QMK VIA
//! build (src/keymap.rs, generated), Vial, and RMK's storage on the external
//! SPI flash, the RGB lighting (src/lighting.rs, with the battery
//! indicators), the radio module for Bluetooth and the 2.4G dongle
//! (src/radio.rs, M6), and sleep (src/power.rs).
//!
//! Storage lives at SPI flash 0x10000-0x1FFFF, and the lighting's at
//! 0x20000-0x21FFF (src/lighting.rs). QMK's wear-levelled EEPROM is in the
//! first 64 KB block (it erases with the 64 KB block command), so this image
//! never touches QMK's settings, and flashing QMK back finds them as they
//! were.
//!
//! Ways to DFU: Fn + Right Shift + Esc (QK_BOOT), Esc held at a cold start
//! (boot-guard), or a panic (boot-guard keeps the message for the next boot).
//!
//! For the bench (tools/bench.py Capture), VIA's id_custom_get_value (0x08)
//! answers on four channels besides Vial's lighting: 0xD1, the scan's
//! diagnostics (src/scan.rs); 0xD2, the message of the panic that ended the
//! previous run; 0xD3 with argument 0x50 ('P'), a deliberate panic, to
//! test the crash policy; 0xD4, the radio link's counters
//! (src/radio.rs); 0xD5, the deepest the stack has been since start-up
//! (`paint_stack`); and 0xD6, the key debounce: argument 1 makes presses
//! count at their first reading (the default), 2 makes them wait out the
//! debounce too (for a chattering switch), anything else just reads it;
//! byte 3 of the answer is 1 while presses wait. A change is saved with the
//! lighting record. 0xD7, the sleep timeout (src/power.rs): argument 0
//! reads it, 1 + n sets it to n (0 never, 1-6: 1, 3, 5, 10, 20, 30 min, 7:
//! 2 min, the default); byte 3 of the answer is the setting; a change is
//! saved with the lighting record. 0xD8 with argument 0x48 ('H') hangs the
//! firmware, to test the watchdog. 0xD9, stop mode while asleep
//! (src/power.rs, off at reset, not saved): argument 0 reads, 1 turns it
//! off, 0x53 ('S') on, 0x44 ('D') on with its progress shown on the Caps
//! and Win-lock LEDs and byte 3 its variant bits (src/stop.rs); byte 3 of the answer is 1 if on, bytes 4-7 and 8-11
//! (little-endian) the stops so far and those a key, the knob or the cable
//! ended, 12-15 the tries that didn't stop (an interrupt was pending), 16
//! the diagnostic's variant bits (0xFF: off). With the `rom-peek` feature,
//! 0xDA, a read-only peek at the ROM bootloader and the information blocks
//! (`peek`, rom/tools/rompeek.py), for reading the ROM out (rom/README.md).
//!
//! Crash policy (boot-guard): a panic resets into this image; a second one
//! before the image has run `STABLE_AFTER` goes to DFU. The software
//! watchdog (src/watchdog.rs) turns a hang into a panic.

#![no_std]
#![no_main]

mod bootloader;
mod keymap;
mod knob;
mod lighting;
mod power;
mod radio;
mod scan;
mod stop;
mod watchdog;

use akko_boot_guard as _;
use embassy_executor::Spawner;
use rmk::config::{BehaviorConfig, DeviceConfig, PositionalConfig, RmkConfig, StorageConfig, VialConfig};
use rmk::host::HostService;
use rmk::keyboard::Keyboard;
use rmk::processor::builtin::led_indicator::KeyboardIndicatorProcessor;
use rmk::types::led_indicator::LedIndicatorType;
use rmk::usb::UsbTransport;
use rmk::KeymapData;
use rmk::keymap::KeyMap;
use rmk::storage::Storage;
use core::future::Future;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy_time::{Duration, Timer};
use wb32_hal::exti::ExtiInput;
use wb32_hal::gpio::{AnyPin, Input, Level, Output, Pins, Pull};
use wb32_hal::qspi::Qspi;
use wb32_hal::spi_nor::{Partition, SharedNor, SpiNor};
use wb32_hal::uart::Uart3;
use wb32_hal::ws2812::Ws2812;
use wb32_hal::{rcc, time_driver, usb};

use crate::keymap::{COL, ENCODER_MAP, KEYMAP, NUM_ENCODER, NUM_LAYER, ROW};

include!(concat!(env!("OUT_DIR"), "/vial_generated.rs"));

const PRIO_TIME: u8 = 0x40;
const PRIO_USB: u8 = 0x60;
const PRIO_EXTI: u8 = 0x80;
/// The LED chain's SPI interrupt: 9, as QMK.
const PRIO_LEDS: u8 = 0x90;
/// The radio module's UART: 8, as QMK (WB32_SERIAL_UART3_PRIORITY).
const PRIO_UART: u8 = 0x80;

/// RMK's storage window on the SPI flash, after QMK's 64 KB block.
const STORAGE_START: u32 = 0x1_0000;
const STORAGE_LEN: u32 = 0x1_0000;
/// SCK = PCLK1 / 32 = 3 MHz, as QMK.
const SPI_DIVISOR: u16 = 32;

/// id_custom_get_value channels (see the top of this file).
const CHANNEL_SCAN: u8 = 0xD1;
const CHANNEL_PANIC: u8 = 0xD2;
const CHANNEL_CRASH: u8 = 0xD3;
const CHANNEL_RADIO: u8 = 0xD4;
const CHANNEL_STACK: u8 = 0xD5;
const CHANNEL_DEBOUNCE: u8 = 0xD6;
const CHANNEL_SLEEP: u8 = 0xD7;
const CHANNEL_HANG: u8 = 0xD8;
const HANG_CONFIRM: u8 = b'H';
const CHANNEL_STOP: u8 = 0xD9;
const STOP_OFF: u8 = 1;
const STOP_CONFIRM: u8 = b'S';
const STOP_DIAG: u8 = b'D';
#[cfg(feature = "rom-peek")]
const CHANNEL_PEEK: u8 = 0xDA;
/// What CHANNEL_PEEK may read: the ROM bootloader (system memory) and the
/// information blocks (RM 3.2.2-3.2.3). Nothing else, so a bad address
/// can't fault, and nothing is ever written.
#[cfg(feature = "rom-peek")]
const PEEK_RANGES: [(u32, u32); 2] = [(0x1FFF_E000, 0x1FFF_F000), (0x1FFF_F000, 0x1FFF_F800)];
#[cfg(feature = "rom-peek")]
const PEEK_MAX: usize = 24;
const DEBOUNCE_EAGER: u8 = 1;
const DEBOUNCE_DEFER: u8 = 2;
const CRASH_CONFIRM: u8 = b'P';
const VIA_GET_VALUE: u8 = 0x08;

/// The USB serial: Vial's magic, so the Vial app finds it. Not RMK's
/// `RMK_BUILD_INFO` ("vial:f64c2b3c;rmk:0.9.0"): the kernel names the
/// battery after it, and upower can't export a name with a ';' (not allowed
/// in a D-Bus object path), so the battery never showed.
const SERIAL: &str = "vial:f64c2b3c-akko-rmk";

/// The keys hs_factory_scan looks for at power-up: (row, col).
const LCTRL: (usize, usize) = (5, 0);
const ESC: (usize, usize) = (0, 0);

/// After this long without a panic, the next panic resets into this image
/// again rather than going to DFU.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The LED chain's colours as sent (the WS2812 driver's), and the lighting
/// state.
#[unsafe(link_section = ".ram2")]
static mut LED_FRAME: MaybeUninit<[[u8; 3]; akko_lighting::layout::LED_COUNT]> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut LIGHTING: MaybeUninit<akko_lighting::Board> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut LIGHTING_RECORD: MaybeUninit<[u8; akko_lighting::record::LEN]> = MaybeUninit::uninit();
/// The SPI flash, shared by RMK's storage and the lighting's.
#[unsafe(link_section = ".ram2")]
static mut NOR: MaybeUninit<SharedNor> = MaybeUninit::uninit();

/// Puts `value` in a .ram2 static (memory.x's RAM2, above the magic word),
/// out of .bss and the main task, which share RAM with the stack.
///
/// # Safety
/// Once per slot.
unsafe fn ram2<T>(slot: *mut MaybeUninit<T>, value: T) -> &'static mut T {
    unsafe { (*slot).write(value) }
}

/// `ram2` for a value that is all zero bytes, zeroed in place rather than
/// built on the stack and copied.
///
/// # Safety
/// Once per slot, and all zero bytes must be a valid `T` (byte arrays).
unsafe fn ram2_zeroed<T>(slot: *mut MaybeUninit<T>) -> &'static mut T {
    unsafe {
        (*slot).as_mut_ptr().write_bytes(0, 1);
        (*slot).assume_init_mut()
    }
}

/// The keymap in RAM and the behaviour settings it borrows (Vial edits
/// change them): 3.4 KB that would otherwise sit in main's future.
#[unsafe(link_section = ".ram2")]
static mut KEYMAP_DATA: MaybeUninit<KeymapData<ROW, COL, NUM_LAYER, NUM_ENCODER>> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut BEHAVIOR: MaybeUninit<BehaviorConfig> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut POSITIONAL: MaybeUninit<PositionalConfig<ROW, COL>> = MaybeUninit::uninit();

/// RMK's storage (about 1 KB with its buffer and caches), built in place by
/// `init_storage`. Returned by value, as rmk's initialize_keymap_and_storage
/// does, it was copied through main's poll frame and the start-up one, which
/// with the first layout write made the stack's deepest point.
#[unsafe(link_section = ".ram2")]
static mut STORAGE: MaybeUninit<Storage<Partition<'static>>> = MaybeUninit::uninit();

/// Opens RMK's storage into STORAGE (rmk's initialize_keymap_and_storage,
/// without returning it). Poll it from an `OwnFrame`.
///
/// # Safety
/// Once.
async unsafe fn init_storage(flash: Partition<'static>, config: &StorageConfig) -> &'static mut Storage<Partition<'static>> {
    let storage = Storage::new(flash, config).await;
    unsafe { (*&raw mut STORAGE).write(storage) }
}

/// Builds the keymap's data and settings in their .ram2 slots; out of line,
/// so the 3.3 KB `KeymapData` is built in a frame that's gone after
/// start-up (see `OwnFrame`).
#[inline(never)]
fn keymap_parts() -> (
    &'static mut KeymapData<ROW, COL, NUM_LAYER, NUM_ENCODER>,
    &'static mut BehaviorConfig,
    &'static PositionalConfig<ROW, COL>,
) {
    // SAFETY: called once, from main.
    unsafe {
        (
            ram2(&raw mut KEYMAP_DATA, KeymapData::new_with_encoder(KEYMAP, ENCODER_MAP)),
            ram2(&raw mut BEHAVIOR, BehaviorConfig::default()),
            ram2(&raw mut POSITIONAL, PositionalConfig::default()),
        )
    }
}

/// The previous run's panic message and its length, taken from boot-guard
/// at start-up, before `custom_get_value` is hooked in; read-only after.
#[unsafe(link_section = ".ram2")]
static mut PANIC_MSG: MaybeUninit<([u8; akko_boot_guard::PANIC_MSG_MAX], usize)> = MaybeUninit::uninit();

/// VIA id_custom_set_value, get_value and save: `buf` is the 32-byte reply,
/// pre-filled with the request <command> <channel> <arg>. Vial's lighting
/// (VialRGB) goes to src/lighting.rs; the rest are get_value channels.
/// CHANNEL_PANIC with arg = offset answers 08 d2 <offset> <total length>
/// and up to 28 bytes of the message.
fn custom_value(buf: &mut [u8]) {
    if lighting::vialrgb(buf) || buf[0] != VIA_GET_VALUE {
        return;
    }
    match buf[1] {
        CHANNEL_SCAN => scan::fill_diag(buf),
        CHANNEL_PANIC if buf.len() >= 32 => {
            // SAFETY: written in main before this hook is installed.
            let (msg, len) = unsafe { (*&raw const PANIC_MSG).assume_init_ref() };
            let off = (buf[2] as usize).min(*len);
            let n = (*len - off).min(28);
            buf[3] = *len as u8;
            buf[4..4 + n].copy_from_slice(&msg[off..off + n]);
            buf[4 + n..32].fill(0);
        }
        CHANNEL_CRASH if buf[2] == CRASH_CONFIRM => panic!("requested over Vial"),
        #[allow(clippy::empty_loop)]
        CHANNEL_HANG if buf[2] == HANG_CONFIRM => loop {},
        CHANNEL_RADIO => radio::fill_diag(buf),
        CHANNEL_DEBOUNCE => {
            let defer = match buf[2] {
                DEBOUNCE_EAGER => Some(false),
                DEBOUNCE_DEFER => Some(true),
                _ => None,
            };
            if let Some(defer) = defer {
                if defer != scan::defer_press() {
                    scan::set_defer_press(defer);
                    lighting::request_save();
                }
            }
            buf[3] = scan::defer_press() as u8;
        }
        CHANNEL_SLEEP => {
            if let Some(n) = buf[2].checked_sub(1) {
                if n != power::setting() {
                    power::set_setting(n);
                    lighting::request_save();
                }
            }
            buf[3] = power::setting();
        }
        CHANNEL_STOP if buf.len() >= 17 => {
            match buf[2] {
                STOP_OFF => power::set_stop_mode(false),
                STOP_CONFIRM => power::set_stop_mode(true),
                STOP_DIAG => power::set_stop_diag(buf[3]),
                _ => {}
            }
            let (stops, wakes, aborts) = power::stop_counts();
            buf[3] = power::stop_mode() as u8;
            buf[4..8].copy_from_slice(&stops.to_le_bytes());
            buf[8..12].copy_from_slice(&wakes.to_le_bytes());
            buf[12..16].copy_from_slice(&aborts.to_le_bytes());
            buf[16] = power::stop_diag().unwrap_or(0xFF);
        }
        #[cfg(feature = "rom-peek")]
        CHANNEL_PEEK if buf.len() >= 32 => peek(buf),
        CHANNEL_STACK => {
            let (used, size) = stack_peak();
            buf[2..6].copy_from_slice(&used.to_le_bytes());
            buf[6..10].copy_from_slice(&size.to_le_bytes());
        }
        _ => {}
    }
}

/// CHANNEL_PEEK: 08 da <address, 4 bytes LE> answers 08 da <address>
/// <status> <count> and `count` bytes from the address. Status 0 is read,
/// 1 refused: the address isn't word-aligned or not in `PEEK_RANGES`. A
/// read stops at the end of its range.
#[cfg(feature = "rom-peek")]
fn peek(buf: &mut [u8]) {
    let addr = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
    buf[6..32].fill(0);
    let Some(&(_, end)) = PEEK_RANGES.iter().find(|&&(start, end)| addr % 4 == 0 && (start..end).contains(&addr)) else {
        buf[6] = 1;
        return;
    };
    let n = ((end - addr) as usize).min(PEEK_MAX);
    for i in (0..n).step_by(4) {
        // SAFETY: an aligned word inside PEEK_RANGES, which is always mapped
        // and read-only.
        let w = unsafe { core::ptr::read_volatile((addr as usize + i) as *const u32) };
        buf[8 + i..12 + i].copy_from_slice(&w.to_le_bytes());
    }
    buf[7] = n as u8;
}

/// What `paint_stack` fills the free stack with.
const STACK_PAINT: u32 = 0xAAAA_AAAA;

unsafe extern "C" {
    /// The top of the stack (flip-link: the bottom of RAM up to here).
    static _stack_start: u32;
}

/// Fills the stack below the current frame with `STACK_PAINT`, so
/// `stack_peak` can find the deepest point it has reached since. Called
/// first thing in main, before any interrupt is enabled.
#[inline(never)]
fn paint_stack() {
    // Leave this function's own frame (and a margin) alone.
    let below = cortex_m::register::msp::read() as usize - 64;
    let mut p = 0x2000_0000 as *mut u32;
    while (p as usize) < below {
        // SAFETY: free stack, below every live frame; interrupts are off.
        unsafe {
            p.write_volatile(STACK_PAINT);
            p = p.add(1);
        }
    }
}

/// (The most stack ever used since `paint_stack`, the stack's size), in
/// bytes: the lowest word that isn't paint any more.
fn stack_peak() -> (u32, u32) {
    let top = &raw const _stack_start as u32;
    let mut a = 0x2000_0000u32;
    // SAFETY: reads of RAM below the stack's top.
    while a < top && unsafe { (a as *const u32).read_volatile() } == STACK_PAINT {
        a += 4;
    }
    (top - a, top - 0x2000_0000)
}

/// Polls `F` from a stack frame of its own. Main never returns, so its
/// poll function's frame (every temporary inlined into it, start-up's
/// included) stays under the whole run; start-up work wrapped in this only
/// takes stack while it runs.
struct OwnFrame<F>(F);

impl<F: Future> Future for OwnFrame<F> {
    type Output = F::Output;

    #[inline(never)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: structural pinning; the field is never moved out.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

/// Takes the previous run's panic message from boot-guard into PANIC_MSG;
/// out of line, for main's frame (see `OwnFrame`).
#[inline(never)]
fn keep_panic_message() {
    let mut msg = [0; akko_boot_guard::PANIC_MSG_MAX];
    let mut buf = [0; akko_boot_guard::PANIC_MSG_MAX];
    let len = akko_boot_guard::take_panic_message(&mut buf).map_or(0, |text| {
        msg[..text.len()].copy_from_slice(text.as_bytes());
        text.len()
    });
    // SAFETY: called once, from main, before anything reads it.
    unsafe { ram2(&raw mut PANIC_MSG, (msg, len)) };
}

/// A task per runnable of ours (see the end of `main`).
macro_rules! runnable_tasks {
    ($($task:ident($ty:ty);)*) => {$(
        #[embassy_executor::task]
        async fn $task(mut r: $ty) -> ! {
            use rmk::core_traits::Runnable as _;
            r.run().await
        }
    )*};
}

runnable_tasks! {
    scan_task(scan::Scan);
    knob_task(knob::Knob);
    caps_task(KeyboardIndicatorProcessor<power::CapsPin>);
    boot_task(bootloader::Bootloader);
    lights_task(lighting::Lighting);
    radio_task(radio::Radio);
    power_task(power::Power);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    paint_stack();
    rcc::init();
    time_driver::init(PRIO_TIME);
    let p = Pins::take().unwrap();
    keep_panic_message();

    // Board pins as 5075b_plus.c's keyboard_post_init_kb; src/lighting.rs
    // switches the LED supply.
    // USB's D+ pull-up, on (low); src/radio.rs turns it off in wireless mode.
    let usb_power = Output::new(p.PB1, Level::Low);
    let led_power = Output::new(p.PA8, Level::Low);
    let led_boost = Output::new(p.PA9, Level::High);
    let aux_led = Output::new(p.PB9, Level::Low);
    // The cable sense, high with a cable in (HS_BAT_CABLE_PIN).
    let cable = Input::new(p.PA7, Pull::None);
    let caps_led = Output::new(p.PC5, Level::High);

    let mut scan = scan::Scan::new(
        [p.PA0.into(), p.PA1.into(), p.PA2.into(), p.PA3.into(), p.PA4.into(), p.PC13.into()],
        [
            AnyPin::from(p.PC0), p.PC1.into(), p.PC2.into(), p.PC3.into(), p.PA6.into(),
            p.PB10.into(), p.PB11.into(), p.PB12.into(), p.PB13.into(), p.PB14.into(),
            p.PA10.into(), p.PC6.into(), p.PC7.into(), p.PC8.into(), p.PC9.into(),
        ],
        p.PC15.into(),
        p.PC14.into(),
        PRIO_EXTI,
    );
    let win_lock_led = Output::new(p.PB0, Level::High); // off (active low)
    let knob = knob::Knob::new(
        ExtiInput::new(p.PB7, Pull::Up, PRIO_EXTI),
        ExtiInput::new(p.PB6, Pull::Up, PRIO_EXTI),
        0,
    );

    let nor = SpiNor::new(Qspi::new(p.PB3, p.PB4, p.PB5, SPI_DIVISOR), Output::new(p.PC12, Level::High));
    // SAFETY: main runs once.
    let nor: &'static SharedNor = unsafe { ram2(&raw mut NOR, SharedNor::new(nor)) };
    let jedec = nor.jedec_id();
    let (Some(flash), Some(lighting_flash)) = (
        nor.partition(STORAGE_START, STORAGE_LEN),
        nor.partition(lighting::STORE_START, lighting::STORE_LEN),
    ) else {
        panic!("SPI flash {:02x} {:02x} {:02x}: too small or not found", jedec[0], jedec[1], jedec[2]);
    };

    let rmk_config = RmkConfig {
        device_config: DeviceConfig {
            vid: 0x342D,
            pid: 0xE4F6,
            manufacturer: "AKKO",
            product_name: "AKKO 5075B Plus (RMK)",
            serial_number: heapless::String::try_from(SERIAL).unwrap(),
        },
        vial_config: VialConfig { insecure: true, ..VialConfig::new(&VIAL_KEYBOARD_ID, &VIAL_KEYBOARD_DEF, &[]) },
        // start_addr 0: the last num_sectors of the window, here all 16 of
        // it. More room means fewer page migrations (each moves every item).
        storage_config: StorageConfig { start_addr: 0, num_sectors: 16, ..Default::default() },
    };

    let (data, behavior, positional) = keymap_parts();
    // SAFETY: main runs once.
    let storage = OwnFrame(unsafe { init_storage(flash, &rmk_config.storage_config) }).await;
    let keymap = OwnFrame(KeyMap::new_from_storage(data, Some(&mut *storage), behavior, positional)).await;

    rmk::host::set_custom_value_hook(custom_value);
    let mut keyboard = Keyboard::new(&keymap);
    let host_service = HostService::new(&keymap, &rmk_config);
    let mut usb = UsbTransport::new(usb::Driver::new(p.PA11, p.PA12, PRIO_USB), rmk_config.device_config)
        .with_host_service(&host_service);
    let caps = KeyboardIndicatorProcessor::new(power::CapsPin::new(caps_led), true, LedIndicatorType::CapsLock);
    let boot = bootloader::Bootloader::new();
    // SAFETY: main runs once.
    let store = OwnFrame(lighting::Store::load(lighting_flash, unsafe { ram2_zeroed(&raw mut LIGHTING_RECORD) })).await;
    // The factory LED test: Left Ctrl held at power-up, without Esc
    // (hs_factory_scan; Esc held is boot-guard's, before this).
    let led_test = scan.is_down(LCTRL.0, LCTRL.1) && !scan.is_down(ESC.0, ESC.1);
    // SAFETY: main runs once.
    let (frame, board) = unsafe {
        (
            ram2_zeroed(&raw mut LED_FRAME),
            // Built in place: it's 2 KB.
            akko_lighting::Board::new_in(
                &mut *&raw mut LIGHTING,
                store.record(),
                lighting::Lighting::recordings(),
                led_test,
                0,
            ),
        )
    };
    // Not configured until RMK says so.
    board.vendor.usb_active = false;
    let flags = store.record().map_or(0, |r| r.flags());
    lighting::set_flags(flags, store.record().map_or(0, |r| r.flags2()));
    power::set_setting(store.record().map_or(akko_lighting::record::SLEEP_DEFAULT, |r| r.sleep()));
    radio::set_saved_flags(flags);
    let lights = lighting::Lighting::new(
        board,
        store,
        Ws2812::new(p.PB15, frame, PRIO_LEDS),
        led_power,
        win_lock_led,
        aux_led,
        cable,
        led_boost,
    );
    rmk::keyboard::set_action_hook(lighting::action_hook);
    let power = power::Power::new();
    let radio = radio::Radio::new(Uart3::new(p.PC10, p.PC11, radio::BAUD, PRIO_UART), usb_power);
    // Vial's "bootloader" button: DFU, as QK_BOOT (src/bootloader.rs).
    rmk::boot::set_bootloader_hook(akko_boot_guard::request_bootloader);

    let stable = async {
        Timer::after(STABLE_AFTER).await;
        akko_boot_guard::mark_stable();
    };
    // Ours run as tasks of their own: a join polls every child each time
    // any of them wakes (about 6k instructions a wake with all of them in
    // it), a task only itself. RMK's borrow the keymap and stay in main's
    // join, which they wake only when there's input or a host request.
    watchdog::start();
    spawner.spawn(scan_task(scan).unwrap());
    spawner.spawn(knob_task(knob).unwrap());
    spawner.spawn(caps_task(caps).unwrap());
    spawner.spawn(boot_task(boot).unwrap());
    spawner.spawn(lights_task(lights).unwrap());
    spawner.spawn(radio_task(radio).unwrap());
    spawner.spawn(power_task(power).unwrap());
    // As run_all!, but each one polled from its own frame (`OwnFrame`):
    // inlined, a join's poll frame is the sum of its children's, and it is
    // under every poll for the whole run.
    use rmk::core_traits::Runnable as _;
    embassy_futures::join::join3(
        rmk::join_all!(
            OwnFrame(keyboard.run()),
            OwnFrame(storage.run()),
            OwnFrame(usb.run()),
            OwnFrame(radio::vial(&host_service)),
        ),
        stable,
        watchdog::heartbeat(),
    )
    .await;
}
