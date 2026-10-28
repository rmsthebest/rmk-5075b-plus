//! The radio module on UART3 (M6): Bluetooth 1-3 and the 2.4G dongle,
//! through the `wireless` crate (QMK's linker/wireless, from the fixed
//! tree), as 5075b_plus.c and wls.c drive it.
//!
//! - The pairing keys are User(0-4): Fn+E/R/T (BT1-3), Fn+Y (2.4G), Fn+U
//!   (USB). A press switches to that device; held 3 s, BT1-3 and 2.4G also
//!   clear the device's pairing and pair anew. As the vendor's deferred
//!   long press, which checks the last key processed when it fires, any
//!   other key in those 3 s cancels it.
//! - While a wireless device is selected, RMK's reports go to the module
//!   (`rmk::channel::set_report_hook`) and the USB D+ pull-up is off (B1
//!   high, USB_POWER_EN_PIN), as QMK stops USB; USB gets an all-up report
//!   first, and the module one when switching back.
//! - 100 ms after start-up: ask the module's version, enable its 30 min
//!   sleep in both modes, then select the saved device.
//! - The charge state (A7 cable, A15 full) goes to the module when it
//!   changes and every 10 s, only while nothing is queued; the battery
//!   level is asked every 3 s. Both go to the lighting's battery
//!   indicators (`battery`).
//! - The host's lock LEDs come from the module while a wireless device is
//!   selected (0 while it isn't connected).
//! - Vial's id_custom_get_value channel 0xD4 answers the link counters
//!   (see `fill_diag`).
//!
//! The selected device and the last BT device are saved with the lighting
//! record's flags (src/lighting.rs).
//!
//! The connection LEDs (37-41) are the lighting's (akko_lighting's
//! board): it takes the device changes from `take_board_event` and the
//! module's state from `module_state` before each frame.
//!
//! Sleep (src/power.rs) asks here: whether a wireless device is selected
//! (`wireless`), whether a pairing key's hold started the pairing
//! (`pairing`), the module told before sleeping (`module_sleep`), the
//! device re-selected on waking (`reselect`); the module's host state
//! frames go to `power::host_state`.
//!
//! Vial through the dongle (md_raw.c): the host's raw HID requests come
//! from the module as raw frames (AF 60), go to a second RMK Vial session
//! (`vial`), and its answers go back as raw frames (AF 61).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_futures::select::{Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use akko_lighting::record;
use rmk::event::{ActionEvent, EventSubscriber};
use rmk::hid::Report;
use rmk::macros::processor;
use rmk::processor::Processor;
use rmk::types::action::Action;
use rmk::types::led_indicator::LedIndicator;
use wb32_hal::gpio::{AnyPin, Input, Output, Port, Pull};
use wb32_hal::uart::Uart3;
use qmk_wireless::frame::{RAW_LEN, devctrl, utf16z};
use qmk_wireless::{Device, DeviceStrings, Event, Link};

pub const BAUD: u32 = 115_200;

/// The keymap's pairing keys (keyboard.json "keycodes" 0-4).
const USER_BT1: u8 = 0;
const USER_2G4: u8 = 3;
const USER_USB: u8 = 4;

/// WLS_KEYCODE_PAIR_TIME.
const PAIR_HOLD_MS: u32 = 3000;
/// wireless_post_task's start-up delay.
const POST_INIT_MS: u32 = 100;
/// HS_CHARGE_STATE_REFRESH.
const CHARGE_REFRESH_MS: u32 = 10_000;
/// WLS_INQUIRY_BAT_TIME, HS_USB_BAT_INQUIRY_TIME.
const BATTERY_POLL_MS: u32 = 3000;
/// Housekeeping while the link is quiet; it's polled every millisecond
/// while a frame waits for its ACK.
const IDLE_POLL_MS: u64 = 50;
/// Time for USB's all-up report to go out before the pull-up is cut.
const USB_RELEASE_MS: u64 = 20;

static MANUFACTURER: [u8; 10] = utf16z("AKKO");
static PRODUCT: [u8; 34] = utf16z("Akko 2.4G VIA KB");

/// config.h's MD_BT*_NAME and MD_DONGLE_PRODUCT; BT4/5 (no keys) and the
/// manufacturer are module.c's defaults from keyboard.json.
static STRINGS: DeviceStrings = DeviceStrings {
    bt_names: [b"Akko 5075-1", b"Akko 5075-2", b"Akko 5075-3", b"AKKO Keyboard BT4", b"AKKO Keyboard BT5"],
    dongle_manufacturer: &MANUFACTURER,
    dongle_product: &PRODUCT,
    vid: 0x342D,
    pid: 0xE4F6,
};

static LINK: Mutex<CriticalSectionRawMutex, RefCell<Link>> = Mutex::new(RefCell::new(Link::new(&STRINGS)));
/// Reports go to the module, not USB.
static WIRELESS: AtomicBool = AtomicBool::new(false);
/// hs_pairing_flag: a pairing key's hold started pairing; a pairing key's
/// press ends it.
static PAIRING: AtomicBool = AtomicBool::new(false);
/// Wakes the task after the report hook queued something.
static POKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// confinfo.devs and last_btdevs, for the lighting record.
static DEVS: AtomicU8 = AtomicU8::new(Device::Usb as u8);
static LAST_BT: AtomicU8 = AtomicU8::new(1);
static UART_ERRORS: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);
/// The module's state as QMK's MD_STATE_* (`qmk_wireless::ModuleState` has
/// the same order), for the lighting.
static MODULE_STATE: AtomicU8 = AtomicU8::new(0);
/// The charge pins as housekeeping last read them: CHARGE_CABLE, CHARGE_FULL.
static CHARGE: AtomicU8 = AtomicU8::new(0);
const CHARGE_CABLE: u8 = 1;
const CHARGE_FULL: u8 = 2;
/// Vial requests from the dongle. Vial waits for each answer, so one is
/// plenty; a second is dropped.
static VIAL_IN: Channel<CriticalSectionRawMutex, [u8; RAW_LEN], 1> = Channel::new();
static BOARD_EVENTS: Mutex<CriticalSectionRawMutex, RefCell<heapless::Deque<BoardEvent, 4>>> =
    Mutex::new(RefCell::new(heapless::Deque::new()));

/// A device change, for the lighting's connection indicator (the board's
/// `devs_change`, `wls_key`, `wls_pair`), as QMK's DEVS_*.
#[derive(Clone, Copy)]
pub enum BoardEvent {
    /// Start-up's select of the saved device.
    Select(u8),
    /// A pairing key pressed.
    Key(u8),
    /// A pairing key held 3 s.
    Pair(u8),
}

fn board_event(e: BoardEvent) {
    // Four between two frames is more than a person can press; drop the rest.
    BOARD_EVENTS.lock(|q| {
        let _ = q.borrow_mut().push_back(e);
    });
}

pub fn take_board_event() -> Option<BoardEvent> {
    BOARD_EVENTS.lock(|q| q.borrow_mut().pop_front())
}

pub fn module_state() -> u8 {
    MODULE_STATE.load(Ordering::Relaxed)
}

/// A wireless device is selected (reports go to the module).
pub fn wireless() -> bool {
    WIRELESS.load(Ordering::Relaxed)
}

/// A pairing key's hold started pairing.
pub fn pairing() -> bool {
    PAIRING.load(Ordering::Relaxed)
}

/// Before sleeping on a timeout or a flat battery: devctrl USB, as the
/// vendor's wls.c sends then.
pub fn module_sleep() {
    LINK.lock(|l| l.borrow_mut().send_devctrl(devctrl::USB));
    POKE.signal(());
}

/// Going to sleep: the keys released to the host, if the link is up (the
/// vendor's clear_keyboard).
pub fn release_keys() {
    if !wireless() {
        return;
    }
    let now = now_ms();
    LINK.lock(|l| {
        let mut l = l.borrow_mut();
        if l.connected() {
            l.send_keyboard([0; 8], now);
            // A system release goes out as consumer 0 too (send_system).
            l.send_consumer(0, now);
        }
    });
    POKE.signal(());
}

/// Going to sleep: a pairing key's longer link timeout ends (the vendor's
/// stop_hook_pre clears hs_pairing_flag).
pub fn end_pairing() {
    PAIRING.store(false, Ordering::Relaxed);
}

/// EE_CLR's reset: the last BT device back to BT1, the selected device kept
/// (eeconfig_confinfo_default).
pub fn reset_saved() {
    LAST_BT.store(1, Ordering::Relaxed);
}

/// On waking: the current device selected again
/// (wireless_devs_change(cur, cur, false)), and its indicator restarted.
pub fn reselect() {
    if !wireless() {
        return;
    }
    let dev = LINK.lock(|l| {
        let mut l = l.borrow_mut();
        let dev = l.device();
        l.select(dev, false);
        dev
    });
    board_event(BoardEvent::Select(dev as u8));
    POKE.signal(());
}

fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

/// The battery for the lighting: the module's level (%), the cable is in,
/// the battery is full.
/// The module has reported the battery level (until then `battery()`'s
/// level is the link's 100).
static BATTERY_KNOWN: AtomicBool = AtomicBool::new(false);

pub fn battery_known() -> bool {
    BATTERY_KNOWN.load(Ordering::Relaxed)
}

pub fn battery() -> (u8, bool, bool) {
    let c = CHARGE.load(Ordering::Relaxed);
    let level = LINK.lock(|l| l.borrow().battery());
    (level, c & CHARGE_CABLE != 0, c & CHARGE_FULL != 0)
}

/// The device bits of the lighting record's flags.
pub fn saved_flags() -> u8 {
    (DEVS.load(Ordering::Relaxed) << record::FLAG_DEVS_SHIFT) & record::FLAG_DEVS_MASK
        | (LAST_BT.load(Ordering::Relaxed) << record::FLAG_LAST_BT_SHIFT) & record::FLAG_LAST_BT_MASK
}

/// The saved device from the lighting record's flags, at start-up.
pub fn set_saved_flags(f: u8) {
    let devs = (f & record::FLAG_DEVS_MASK) >> record::FLAG_DEVS_SHIFT;
    if Device::from_u8(devs).is_some() {
        DEVS.store(devs, Ordering::Relaxed);
    }
    let bt = (f & record::FLAG_LAST_BT_MASK) >> record::FLAG_LAST_BT_SHIFT;
    LAST_BT.store(if bt == 0 { 1 } else { bt }, Ordering::Relaxed);
}

/// RMK's report hook: while a wireless device is selected the module
/// takes every report (wireless.c's host driver).
fn report_hook(report: &Report) -> bool {
    if !WIRELESS.load(Ordering::Relaxed) {
        return false;
    }
    let now = now_ms();
    LINK.lock(|l| {
        let mut l = l.borrow_mut();
        match report {
            Report::KeyboardReport(k) => {
                let mut r = [0; 8];
                r[0] = k.modifier;
                r[2..].copy_from_slice(&k.keycodes);
                l.send_keyboard(r, now);
            }
            Report::MediaKeyboardReport(m) => l.send_consumer(m.usage_id, now),
            Report::SystemControlReport(s) => l.send_system(s.usage_id as u16, now),
            // buttons, x, y, horizontal, vertical (wireless_send_mouse).
            Report::MouseReport(m) => l.send_mouse([m.buttons, m.x as u8, m.y as u8, m.pan as u8, m.wheel as u8], now),
        }
    });
    POKE.signal(());
    true
}

/// Vial channel 0xD4: the link counters (`qmk_wireless::Stats`, 16 bytes from
/// buf[2]), then the UART's receive errors (u16), the module's state,
/// battery %, firmware version, and the selected device.
pub fn fill_diag(buf: &mut [u8]) {
    if buf.len() < 24 {
        return;
    }
    LINK.lock(|l| {
        let l = l.borrow();
        buf[2..18].copy_from_slice(&l.stats.to_le_bytes());
        buf[18..20].copy_from_slice(&UART_ERRORS.load(Ordering::Relaxed).to_le_bytes());
        buf[20] = l.state() as u8;
        buf[21] = l.battery();
        buf[22] = l.version();
        buf[23] = l.device() as u8;
    });
}

/// Vial through the dongle: RMK's Vial session, as USB's, over the
/// module's raw frames. Runs for good; requests only come while a
/// wireless device is selected.
pub async fn vial(service: &rmk::host::HostService<'_>) -> ! {
    let mut rx = VialRx { buf: [0; RAW_LEN], at: RAW_LEN };
    let mut tx = VialTx { buf: [0; RAW_LEN], len: 0 };
    loop {
        // Only returns on an error, and ours never fail.
        service.run_session(&mut rx, &mut tx).await;
    }
}

/// The requests, as a byte stream: one 32-byte report at a time.
struct VialRx {
    buf: [u8; RAW_LEN],
    /// Read up to here; RAW_LEN when it's all been read.
    at: usize,
}

impl embedded_io_async::ErrorType for VialRx {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Read for VialRx {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.at == RAW_LEN {
            self.buf = VIAL_IN.receive().await;
            self.at = 0;
        }
        let n = buf.len().min(RAW_LEN - self.at);
        buf[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// The answers: each 32 bytes written go to the module as one raw frame.
struct VialTx {
    buf: [u8; RAW_LEN],
    len: usize,
}

impl embedded_io_async::ErrorType for VialTx {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Write for VialTx {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let n = buf.len().min(RAW_LEN - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&buf[..n]);
        self.len += n;
        if self.len == RAW_LEN {
            self.len = 0;
            LINK.lock(|l| l.borrow_mut().send_raw(self.buf));
            POKE.signal(());
        }
        Ok(n)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[processor(subscribe = [ActionEvent])]
#[::rmk::macros::runnable_generated]
pub struct Radio {
    uart: Uart3,
    usb_power: Output,
    cable: Input,
    full: Input,
    /// The device the reports go to (QMK's transport): USB or the module.
    wireless: bool,
    post_init_at: Option<u32>,
    /// A pairing key held: the device, and when the hold fires.
    hold: Option<(Device, u32)>,
    charge_sent: Option<(u8, u32)>,
    battery_asked: u32,
    leds: u8,
}

impl Radio {
    /// `usb_power` is B1 (low: USB's D+ pull-up on).
    pub fn new(uart: Uart3, usb_power: Output) -> Self {
        rmk::channel::set_report_hook(report_hook);
        // A7 is also the lighting's (src/lighting.rs); both only read it.
        let cable = Input::new(unsafe { AnyPin::steal(Port::A, 7) }, Pull::None);
        // Pulled up, as QMK's setPinInputHigh(BAT_FULL_PIN): the charger
        // only pulls it low.
        let full = Input::new(unsafe { AnyPin::steal(Port::A, 15) }, Pull::Up);
        Radio {
            uart,
            usb_power,
            cable,
            full,
            wireless: false,
            post_init_at: Some(now_ms().wrapping_add(POST_INIT_MS)),
            hold: None,
            charge_sent: None,
            battery_asked: now_ms(),
            leds: 0,
        }
    }

    async fn on_action_event(&mut self, e: ActionEvent) {
        let pressed = e.keyboard_event.pressed;
        let dev = match e.action {
            Action::User(n @ USER_BT1..=USER_USB) => match n {
                USER_2G4 => Device::Dongle,
                USER_USB => Device::Usb,
                n => Device::from_u8(n - USER_BT1 + 1).unwrap(),
            },
            _ => {
                // keycode_shadow: the long press now sees another key.
                self.hold = None;
                return;
            }
        };
        if !pressed {
            self.hold = None;
            return;
        }
        board_event(BoardEvent::Key(dev as u8));
        PAIRING.store(false, Ordering::Relaxed);
        crate::power::link_kick();
        let current = LINK.lock(|l| l.borrow().device());
        if current != dev {
            self.change(dev, false).await;
        } else if dev.is_wireless() {
            // The current device again: selected again, so the module
            // reconnects (WLS_KEYCODE_WAKEUP's suspend_wakeup_init).
            LINK.lock(|l| l.borrow_mut().select(dev, false));
        }
        if dev != Device::Usb && self.hold.is_none() {
            self.hold = Some((dev, now_ms().wrapping_add(PAIR_HOLD_MS)));
        }
    }

    /// wireless_devs_change(current, dev, reset).
    async fn change(&mut self, dev: Device, reset: bool) {
        if dev.is_wireless() != self.wireless {
            self.set_transport(dev.is_wireless()).await;
        }
        LINK.lock(|l| l.borrow_mut().select(dev, reset));
        DEVS.store(dev as u8, Ordering::Relaxed);
        if matches!(dev, Device::Bt1 | Device::Bt2 | Device::Bt3) {
            LAST_BT.store(dev as u8, Ordering::Relaxed);
        }
    }

    /// set_transport: hands the reports between USB and the module.
    async fn set_transport(&mut self, wireless: bool) {
        self.wireless = wireless;
        if wireless {
            rmk::channel::release_usb_reports();
            WIRELESS.store(true, Ordering::Relaxed);
            crate::power::refresh_caps();
            Timer::after_millis(USB_RELEASE_MS).await;
            self.usb_power.set_high();
        } else {
            let now = now_ms();
            LINK.lock(|l| {
                let mut l = l.borrow_mut();
                if l.connected() {
                    l.send_keyboard([0; 8], now);
                }
            });
            WIRELESS.store(false, Ordering::Relaxed);
            crate::power::refresh_caps();
            self.usb_power.set_low();
            self.set_leds(0);
        }
    }

    fn set_leds(&mut self, leds: u8) {
        if leds != self.leds {
            self.leds = leds;
            rmk::hid::set_host_leds(LedIndicator::from_bits(leds));
        }
    }

    /// Everything that isn't an event: the UART both ways, the link's
    /// timers, and the board's.
    async fn service(&mut self) {
        let now = now_ms();
        let uart = &self.uart;
        let mut tx = |b: &[u8]| {
            uart.write(b);
        };
        LINK.lock(|l| {
            let mut l = l.borrow_mut();
            // md_main_task: send, then read what came.
            l.poll(now, &mut tx);
            if uart.rx_empty() {
                l.rx_idle(now);
            }
            while let Some(b) = uart.read() {
                match l.receive(b, now, &mut tx) {
                    Some(Event::HostState { resume }) => crate::power::host_state(resume),
                    Some(Event::Battery(_)) => BATTERY_KNOWN.store(true, Ordering::Relaxed),
                    Some(Event::Raw(r)) => {
                        let _ = VIAL_IN.try_send(r);
                    }
                    _ => {}
                }
            }
        });
        UART_ERRORS.store(self.uart.rx_errors(), Ordering::Relaxed);
        MODULE_STATE.store(LINK.lock(|l| l.borrow().state()) as u8, Ordering::Relaxed);

        if self.post_init_at.is_some_and(|t| now.wrapping_sub(t) as i32 >= 0) {
            self.post_init_at = None;
            LINK.lock(|l| {
                let mut l = l.borrow_mut();
                l.send_devctrl(devctrl::FW_VERSION);
                l.send_devctrl(devctrl::SLEEP_BT_EN);
                l.send_devctrl(devctrl::SLEEP_2G4_EN);
            });
            // The battery as soon as the link is free, not 3 s on: on USB
            // power a plug-in starts us, and the host reads the battery
            // (GET_REPORT) within about a second (src/lighting.rs).
            self.battery_asked = now.wrapping_sub(BATTERY_POLL_MS + 1);
            let dev = Device::from_u8(DEVS.load(Ordering::Relaxed)).unwrap_or(Device::Usb);
            board_event(BoardEvent::Select(dev as u8));
            self.change(dev, false).await;
        }

        if let Some((dev, at)) = self.hold
            && now.wrapping_sub(at) as i32 >= 0
        {
            self.hold = None;
            PAIRING.store(true, Ordering::Relaxed);
            board_event(BoardEvent::Pair(dev as u8));
            self.change(dev, true).await;
        }

        self.housekeeping(now);

        if self.wireless {
            let leds = LINK.lock(|l| l.borrow().indicator());
            self.set_leds(leds);
        }
    }

    /// hs_housekeeping_task's module traffic.
    fn housekeeping(&mut self, now: u32) {
        let charging = self.cable.is_high();
        let full = self.full.is_high();
        CHARGE.store(
            if charging { CHARGE_CABLE } else { 0 } | if full { CHARGE_FULL } else { 0 },
            Ordering::Relaxed,
        );
        // The module hears nothing before the post-init (version, sleep,
        // device), as QMK's start-up order has it.
        if self.post_init_at.is_some() {
            return;
        }
        let state = match (charging, full) {
            (true, true) => devctrl::CHARGING_DONE,
            (true, false) => devctrl::CHARGING,
            (false, _) => devctrl::CHARGING_STOP,
        };
        LINK.lock(|l| {
            let mut l = l.borrow_mut();
            let due = self
                .charge_sent
                .is_none_or(|(sent, at)| sent != state || now.wrapping_sub(at) > CHARGE_REFRESH_MS);
            if due && !l.busy() {
                self.charge_sent = Some((state, now));
                l.send_devctrl(state);
            }
            if now.wrapping_sub(self.battery_asked) > BATTERY_POLL_MS && l.inquire_battery() {
                self.battery_asked = now;
            }
        });
    }
}

impl rmk::core_traits::Runnable for Radio {
    async fn run(&mut self) -> ! {
        let mut sub = Self::subscriber();
        loop {
            let now = now_ms();
            let until = |t: u32| (t.wrapping_sub(now) as i32).max(0) as u64;
            let mut wait = if LINK.lock(|l| l.borrow().busy()) { 1 } else { IDLE_POLL_MS };
            for t in [self.post_init_at, self.hold.map(|(_, t)| t)].into_iter().flatten() {
                wait = wait.min(until(t));
            }
            let event = match select3(
                sub.next_event(),
                self.uart.wait_readable(),
                select(POKE.wait(), Timer::after_millis(wait)),
            )
            .await
            {
                Either3::First(e) => Some(e),
                _ => None,
            };
            if let Some(e) = event {
                self.process(e).await;
            }
            self.service().await;
        }
    }
}
