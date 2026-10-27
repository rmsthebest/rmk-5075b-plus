//! The keyboard firmware: RMK over USB (M4).
//!
//! Keys (src/scan.rs), the knob (src/knob.rs), the keymap from the QMK VIA
//! build (src/keymap.rs, generated), Vial, and RMK's storage on the external
//! SPI flash. Lighting (M5), wireless (M6) and battery/sleep (M7) come later;
//! until then their keys are User(n) and do nothing.
//!
//! Storage lives at SPI flash 0x10000-0x1FFFF. QMK's wear-levelled EEPROM is
//! in the first 64 KB block (it erases with the 64 KB block command), so this
//! image never touches QMK's settings, and flashing QMK back finds them as
//! they were.
//!
//! Ways to DFU: Fn + Right Shift + Esc (QK_BOOT), Esc held at a cold start
//! (boot-guard), or a panic (boot-guard keeps the message for the next boot).

#![no_std]
#![no_main]

mod bootloader;
mod keymap;
mod knob;
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
use wb32_hal::exti::ExtiInput;
use wb32_hal::gpio::{AnyPin, Level, Output, Pins, Pull};
use wb32_hal::qspi::Qspi;
use wb32_hal::spi_nor::SpiNor;
use wb32_hal::{rcc, time_driver, usb};

use crate::keymap::{COL, ENCODER_MAP, KEYMAP, NUM_ENCODER, NUM_LAYER, ROW};

include!(concat!(env!("OUT_DIR"), "/vial_generated.rs"));

const PRIO_TIME: u8 = 0x40;
const PRIO_USB: u8 = 0x60;
const PRIO_EXTI: u8 = 0x80;

/// RMK's storage window on the SPI flash, after QMK's 64 KB block.
const STORAGE_START: u32 = 0x1_0000;
const STORAGE_LEN: u32 = 0x1_0000;
/// SCK = PCLK1 / 32 = 3 MHz, as QMK.
const SPI_DIVISOR: u16 = 32;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    rcc::init();
    time_driver::init(PRIO_TIME);
    let p = Pins::take().unwrap();

    // Board pins as 5075b_plus.c's keyboard_post_init_kb, lighting off.
    let _usb_power = Output::new(p.PB1, Level::Low);
    let _led_power = Output::new(p.PA8, Level::Low);
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
        Output::new(p.PB0, Level::High), // Win-lock LED, off (active low)
    );
    let knob = knob::Knob::new(
        ExtiInput::new(p.PB7, Pull::Up, PRIO_EXTI),
        ExtiInput::new(p.PB6, Pull::Up, PRIO_EXTI),
        0,
    );

    let nor = SpiNor::new(Qspi::new(p.PB3, p.PB4, p.PB5, SPI_DIVISOR), Output::new(p.PC12, Level::High));
    let jedec = nor.jedec_id();
    let Some(flash) = nor.partition(STORAGE_START, STORAGE_LEN) else {
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
    let (keymap, mut storage) =
        initialize_keymap_and_storage(&mut data, flash, &rmk_config.storage_config, &mut behavior, &positional).await;

    rmk::host::set_custom_get_value_hook(scan::fill_diag);
    let mut scan = scan;
    let mut knob = knob;
    let mut keyboard = Keyboard::new(&keymap);
    let host_service = HostService::new(&keymap, &rmk_config);
    let mut usb = UsbTransport::new(usb::Driver::new(p.PA11, p.PA12, PRIO_USB), rmk_config.device_config)
        .with_host_service(&host_service);
    let mut caps = KeyboardIndicatorProcessor::new(caps_led, true, LedIndicatorType::CapsLock);
    let mut boot = bootloader::Bootloader::new();

    run_all!(scan, knob, keyboard, storage, usb, caps, boot).await;
}
