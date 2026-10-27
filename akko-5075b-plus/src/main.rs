//! The keyboard firmware: RMK over USB (M4).
//!
//! Keys (src/scan.rs), the knob (src/knob.rs), the keymap from the QMK VIA
//! build (src/keymap.rs, generated), Vial, and RMK's storage on the external
//! SPI flash, and the RGB lighting (src/lighting.rs). Wireless (M6) and
//! battery/sleep (M7) come later; until then their keys are User(n) and do
//! nothing.
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
//! answers on three channels besides Vial's lighting: 0xD1, the scan's
//! diagnostics (src/scan.rs); 0xD2, the message of the panic that ended the
//! previous run; and 0xD3 with argument 0x50 ('P'), a deliberate panic, to
//! test the crash policy.
//!
//! Crash policy (boot-guard): a panic resets into this image; a second one
//! before the image has run `STABLE_AFTER` goes to DFU.

#![no_std]
#![no_main]

mod bootloader;
mod keymap;
mod knob;
mod lighting;
mod scan;

use boot_guard as _;
use embassy_executor::Spawner;
use rmk::config::{BehaviorConfig, DeviceConfig, PositionalConfig, RMK_BUILD_INFO, RmkConfig, StorageConfig, VialConfig};
use rmk::host::HostService;
use rmk::keyboard::Keyboard;
use rmk::processor::builtin::led_indicator::KeyboardIndicatorProcessor;
use rmk::types::led_indicator::LedIndicatorType;
use rmk::usb::UsbTransport;
use rmk::{KeymapData, initialize_keymap_and_storage, run_all};
use core::future::Future;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy_time::{Duration, Timer};
use wb32_hal::exti::ExtiInput;
use wb32_hal::gpio::{AnyPin, Level, Output, Pins, Pull};
use wb32_hal::qspi::Qspi;
use wb32_hal::spi_nor::{SharedNor, SpiNor};
use wb32_hal::ws2812::{self, Ws2812};
use wb32_hal::{rcc, time_driver, usb};

use crate::keymap::{COL, ENCODER_MAP, KEYMAP, NUM_ENCODER, NUM_LAYER, ROW};

include!(concat!(env!("OUT_DIR"), "/vial_generated.rs"));

const PRIO_TIME: u8 = 0x40;
const PRIO_USB: u8 = 0x60;
const PRIO_EXTI: u8 = 0x80;
/// The LED chain's SPI interrupt: 9, as QMK.
const PRIO_LEDS: u8 = 0x90;

/// RMK's storage window on the SPI flash, after QMK's 64 KB block.
const STORAGE_START: u32 = 0x1_0000;
const STORAGE_LEN: u32 = 0x1_0000;
/// SCK = PCLK1 / 32 = 3 MHz, as QMK.
const SPI_DIVISOR: u16 = 32;

/// id_custom_get_value channels (see the top of this file).
const CHANNEL_SCAN: u8 = 0xD1;
const CHANNEL_PANIC: u8 = 0xD2;
const CHANNEL_CRASH: u8 = 0xD3;
const CRASH_CONFIRM: u8 = b'P';
const VIA_GET_VALUE: u8 = 0x08;

/// After this long without a panic, the next panic resets into this image
/// again rather than going to DFU.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The LED chain's encoded frame, and the lighting state.
const LED_FRAME_LEN: usize = ws2812::frame_len(lighting_crate::layout::LED_COUNT);
#[unsafe(link_section = ".ram2")]
static mut LED_FRAME: MaybeUninit<[u8; LED_FRAME_LEN]> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut LIGHTING: MaybeUninit<lighting_crate::Board> = MaybeUninit::uninit();
#[unsafe(link_section = ".ram2")]
static mut LIGHTING_RECORD: MaybeUninit<[u8; lighting_crate::record::LEN]> = MaybeUninit::uninit();
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

/// `ram2` for a byte array, zeroed in place rather than built on the stack
/// and copied.
///
/// # Safety
/// Once per slot.
unsafe fn ram2_zeroed<const N: usize>(slot: *mut MaybeUninit<[u8; N]>) -> &'static mut [u8; N] {
    unsafe {
        (*slot).as_mut_ptr().write_bytes(0, 1);
        (*slot).assume_init_mut()
    }
}

/// The previous run's panic message and its length, taken from boot-guard
/// at start-up, before `custom_get_value` is hooked in; read-only after.
#[unsafe(link_section = ".ram2")]
static mut PANIC_MSG: MaybeUninit<([u8; boot_guard::PANIC_MSG_MAX], usize)> = MaybeUninit::uninit();

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
        _ => {}
    }
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
    let mut msg = [0; boot_guard::PANIC_MSG_MAX];
    let mut buf = [0; boot_guard::PANIC_MSG_MAX];
    let len = boot_guard::take_panic_message(&mut buf).map_or(0, |text| {
        msg[..text.len()].copy_from_slice(text.as_bytes());
        text.len()
    });
    // SAFETY: called once, from main, before anything reads it.
    unsafe { ram2(&raw mut PANIC_MSG, (msg, len)) };
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    rcc::init();
    time_driver::init(PRIO_TIME);
    let p = Pins::take().unwrap();
    keep_panic_message();

    // Board pins as 5075b_plus.c's keyboard_post_init_kb; src/lighting.rs
    // switches the LED supply.
    let _usb_power = Output::new(p.PB1, Level::Low);
    let led_power = Output::new(p.PA8, Level::Low);
    let _led_boost = Output::new(p.PA9, Level::High);
    let _aux_led = Output::new(p.PB9, Level::Low);
    let caps_led = Output::new(p.PC5, Level::High);

    let scan = scan::Scan::new(
        [p.PA0.into(), p.PA1.into(), p.PA2.into(), p.PA3.into(), p.PA4.into(), p.PC13.into()],
        [
            AnyPin::from(p.PC0), p.PC1.into(), p.PC2.into(), p.PC3.into(), p.PA6.into(),
            p.PB10.into(), p.PB11.into(), p.PB12.into(), p.PB13.into(), p.PB14.into(),
            p.PA10.into(), p.PC6.into(), p.PC7.into(), p.PC8.into(), p.PC9.into(),
        ],
        p.PC15.into(),
        p.PC14.into(),
    );
    let _win_lock_led = Output::new(p.PB0, Level::High); // off (active low)
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
            serial_number: RMK_BUILD_INFO,
        },
        vial_config: VialConfig { insecure: true, ..VialConfig::new(&VIAL_KEYBOARD_ID, &VIAL_KEYBOARD_DEF, &[]) },
        // start_addr 0: the last num_sectors of the window.
        storage_config: StorageConfig { start_addr: 0, num_sectors: 4, ..Default::default() },
    };

    let mut behavior = BehaviorConfig::default();
    let positional = PositionalConfig::<ROW, COL>::default();
    let mut data = KeymapData::<ROW, COL, NUM_LAYER, NUM_ENCODER>::new_with_encoder(KEYMAP, ENCODER_MAP);
    let (keymap, mut storage) = OwnFrame(initialize_keymap_and_storage(
        &mut data,
        flash,
        &rmk_config.storage_config,
        &mut behavior,
        &positional,
    ))
    .await;

    rmk::host::set_custom_value_hook(custom_value);
    let mut scan = scan;
    let mut knob = knob;
    let mut keyboard = Keyboard::new(&keymap);
    let host_service = HostService::new(&keymap, &rmk_config);
    let mut usb = UsbTransport::new(usb::Driver::new(p.PA11, p.PA12, PRIO_USB), rmk_config.device_config)
        .with_host_service(&host_service);
    let mut caps = KeyboardIndicatorProcessor::new(caps_led, true, LedIndicatorType::CapsLock);
    let mut boot = bootloader::Bootloader::new();
    // SAFETY: main runs once.
    let store = OwnFrame(lighting::Store::load(lighting_flash, unsafe { ram2_zeroed(&raw mut LIGHTING_RECORD) })).await;
    // SAFETY: main runs once.
    let (frame, board) = unsafe {
        (
            ram2_zeroed(&raw mut LED_FRAME),
            // Built in place: it's 2 KB.
            lighting_crate::Board::new_in(
                &mut *&raw mut LIGHTING,
                store.record(),
                lighting::Lighting::recordings(),
                false,
                0,
            ),
        )
    };
    // Not configured until RMK says so.
    board.vendor.usb_active = false;
    let mut lights = lighting::Lighting::new(board, store, Ws2812::new(p.PB15, frame, PRIO_LEDS), led_power);
    rmk::keyboard::set_action_hook(lighting::action_hook);

    let stable = async {
        Timer::after(STABLE_AFTER).await;
        boot_guard::mark_stable();
    };
    embassy_futures::join::join(run_all!(scan, knob, keyboard, storage, usb, caps, boot, lights), stable).await;
}
