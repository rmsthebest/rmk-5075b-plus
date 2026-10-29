//! GPIO ports A-D.
//!
//! WB32 pin configuration goes through a per-port write mask: config
//! registers (MODER, OTYPER, OSPEEDR, PUPDR, CURRENT, AFRL/H) only change for
//! pins whose CFGMSK bit is 0. [`set_mode`] does what ChibiOS's
//! `_pal_lld_setgroupmode` does: CFGMSK = !pin, then write each register with
//! the field value repeated for every pin. It runs in a critical section
//! because CFGMSK is shared by the whole port.

use core::convert::Infallible;

use wb32_pac::{gpio, rcc};

use crate::rcc as clocks;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Port {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
}

impl Port {
    fn regs(self) -> gpio::Gpio {
        match self {
            Port::A => wb32_pac::GPIOA,
            Port::B => wb32_pac::GPIOB,
            Port::C => wb32_pac::GPIOC,
            Port::D => wb32_pac::GPIOD,
        }
    }

    fn enable_clock(self) {
        clocks::enable_apb1(|w: &mut rcc::regs::Apb1enr| match self {
            Port::A => w.set_gpioaen(true),
            Port::B => w.set_gpioben(true),
            Port::C => w.set_gpiocen(true),
            Port::D => w.set_gpioden(true),
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Low,
    High,
}

impl From<bool> for Level {
    fn from(high: bool) -> Self {
        if high { Level::High } else { Level::Low }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Pull {
    None = 0,
    Up = 1,
    Down = 2,
}

/// Pin configuration; field values as in the reference manual / ChibiOS
/// `PAL_WB32_*`. `Default` is a floating input.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mode {
    /// 0 input, 1 output, 2 alternate function, 3 analog.
    pub moder: u8,
    pub open_drain: bool,
    /// 0 high, 1 low (ChibiOS `PAL_WB32_OSPEED_*`).
    pub speed: u8,
    pub pull: u8,
    pub af: u8,
    /// Drive strength level 0-3.
    pub current: u8,
}

impl Mode {
    pub const fn input(pull: Pull) -> Self {
        Mode { moder: 0, open_drain: false, speed: 0, pull: pull as u8, af: 0, current: 0 }
    }
    pub const fn output() -> Self {
        Mode { moder: 1, ..Mode::input(Pull::None) }
    }
    pub const fn output_open_drain() -> Self {
        Mode { open_drain: true, ..Mode::output() }
    }
    pub const fn alternate(af: u8) -> Self {
        Mode { moder: 2, af, ..Mode::input(Pull::None) }
    }
    pub const fn analog() -> Self {
        Mode { moder: 3, ..Mode::input(Pull::None) }
    }
}

/// A pin chosen at run time. Not `Copy`: whoever holds it owns the pin, and
/// may change its mode and level.
#[derive(PartialEq, Eq, Debug)]
pub struct AnyPin {
    port: Port,
    pin: u8,
}

impl AnyPin {
    /// # Safety
    /// The caller must make sure nothing else drives or reconfigures this pin.
    pub const unsafe fn steal(port: Port, pin: u8) -> Self {
        assert!(pin < 16);
        AnyPin { port, pin }
    }

    pub fn port(&self) -> Port {
        self.port
    }
    pub fn pin(&self) -> u8 {
        self.pin
    }

    fn mask(&self) -> u32 {
        1 << self.pin
    }

    pub fn set_mode(&self, mode: Mode) {
        set_mode(self.port, self.mask(), mode)
    }

    pub fn set_level(&self, level: Level) {
        let r = self.port.regs();
        match level {
            Level::High => r.bsrr().write_value(gpio::regs::Bsrr(self.mask())),
            Level::Low => r.bsrr().write_value(gpio::regs::Bsrr(self.mask() << 16)),
        }
    }

    pub fn is_high(&self) -> bool {
        self.port.regs().idr().read().0 & self.mask() != 0
    }

    pub fn is_set_high(&self) -> bool {
        self.port.regs().odr().read().0 & self.mask() != 0
    }
}

/// Configures every pin in `mask` of `port` (ChibiOS `_pal_lld_setgroupmode`).
/// Enables the port clock first.
pub fn set_mode(port: Port, mask: u32, mode: Mode) {
    port.enable_clock();
    let r = port.regs();
    let all = |v: u8, width: u32| -> u32 {
        let per_pin = match width {
            1 => 0xFFFF_FFFF,
            2 => 0x5555_5555,
            _ => 0x1111_1111,
        };
        v as u32 * per_pin
    };
    critical_section::with(|_| {
        r.cfgmsk().write_value(gpio::regs::Cfgmsk(!mask & 0xFFFF));
        r.moder().write_value(gpio::regs::Moder(all(mode.moder & 3, 2)));
        r.otyper().write_value(gpio::regs::Otyper(all(mode.open_drain as u8, 1)));
        r.ospeedr().write_value(gpio::regs::Ospeedr(all(mode.speed & 3, 2)));
        r.pupdr().write_value(gpio::regs::Pupdr(all(mode.pull & 3, 2)));
        r.current().write_value(gpio::regs::Current(all(mode.current & 3, 2)));
        let afr = all(mode.af & 0xF, 4);
        r.afrl().write_value(gpio::regs::Afrl(afr));
        r.afrh().write_value(gpio::regs::Afrh(afr));
    });
}

/// Push-pull output.
pub struct Output {
    pin: AnyPin,
}

impl Output {
    /// Sets the level before switching the pin to output, so it never glitches.
    /// The port clock goes on first: BSRR writes are lost while it is off.
    pub fn new(pin: impl Into<AnyPin>, initial: Level) -> Self {
        let pin = pin.into();
        pin.port.enable_clock();
        pin.set_level(initial);
        pin.set_mode(Mode::output());
        Output { pin }
    }

    pub fn set_high(&mut self) {
        self.pin.set_level(Level::High)
    }
    pub fn set_low(&mut self) {
        self.pin.set_level(Level::Low)
    }
    pub fn set_level(&mut self, level: Level) {
        self.pin.set_level(level)
    }
    pub fn toggle(&mut self) {
        self.pin.set_level((!self.pin.is_set_high()).into())
    }
    pub fn is_set_high(&self) -> bool {
        self.pin.is_set_high()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.pin.set_mode(Mode::input(Pull::None));
    }
}

pub struct Input {
    pin: AnyPin,
}

impl Input {
    pub fn new(pin: impl Into<AnyPin>, pull: Pull) -> Self {
        let pin = pin.into();
        pin.set_mode(Mode::input(pull));
        Input { pin }
    }

    pub fn is_high(&self) -> bool {
        self.pin.is_high()
    }
    pub fn is_low(&self) -> bool {
        !self.pin.is_high()
    }
    pub fn level(&self) -> Level {
        self.pin.is_high().into()
    }
    pub fn pin(&self) -> &AnyPin {
        &self.pin
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.pin.set_mode(Mode::input(Pull::None));
    }
}

impl embedded_hal::digital::ErrorType for Output {
    type Error = Infallible;
}

impl embedded_hal::digital::OutputPin for Output {
    fn set_high(&mut self) -> Result<(), Infallible> {
        Output::set_high(self);
        Ok(())
    }
    fn set_low(&mut self) -> Result<(), Infallible> {
        Output::set_low(self);
        Ok(())
    }
}

impl embedded_hal::digital::StatefulOutputPin for Output {
    fn is_set_high(&mut self) -> Result<bool, Infallible> {
        Ok(Output::is_set_high(self))
    }
    fn is_set_low(&mut self) -> Result<bool, Infallible> {
        Ok(!Output::is_set_high(self))
    }
}

impl embedded_hal::digital::ErrorType for Input {
    type Error = Infallible;
}

impl embedded_hal::digital::InputPin for Input {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(Input::is_high(self))
    }
    fn is_low(&mut self) -> Result<bool, Infallible> {
        Ok(Input::is_low(self))
    }
}

macro_rules! pins {
    ($($name:ident: $port:ident $pin:literal),* $(,)?) => {
        $(
            #[allow(non_camel_case_types)]
            pub struct $name { _private: () }
            impl From<$name> for AnyPin {
                fn from(_: $name) -> AnyPin {
                    AnyPin { port: Port::$port, pin: $pin }
                }
            }
        )*

        /// Every GPIO pin, each obtainable once via [`Pins::take`].
        #[allow(non_snake_case)]
        pub struct Pins {
            $(pub $name: $name,)*
        }

        impl Pins {
            /// Returns the pins on the first call, `None` afterwards.
            pub fn take() -> Option<Self> {
                static TAKEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
                critical_section::with(|_| {
                    if TAKEN.load(core::sync::atomic::Ordering::Relaxed) {
                        return None;
                    }
                    TAKEN.store(true, core::sync::atomic::Ordering::Relaxed);
                    Some(Pins { $($name: $name { _private: () },)* })
                })
            }
        }
    };
}

pins! {
    PA0: A 0, PA1: A 1, PA2: A 2, PA3: A 3, PA4: A 4, PA5: A 5, PA6: A 6, PA7: A 7,
    PA8: A 8, PA9: A 9, PA10: A 10, PA11: A 11, PA12: A 12, PA13: A 13, PA14: A 14, PA15: A 15,
    PB0: B 0, PB1: B 1, PB2: B 2, PB3: B 3, PB4: B 4, PB5: B 5, PB6: B 6, PB7: B 7,
    PB8: B 8, PB9: B 9, PB10: B 10, PB11: B 11, PB12: B 12, PB13: B 13, PB14: B 14, PB15: B 15,
    PC0: C 0, PC1: C 1, PC2: C 2, PC3: C 3, PC4: C 4, PC5: C 5, PC6: C 6, PC7: C 7,
    PC8: C 8, PC9: C 9, PC10: C 10, PC11: C 11, PC12: C 12, PC13: C 13, PC14: C 14, PC15: C 15,
}
// No port D pins: PD0/PD1 are the 12 MHz crystal, owned by rcc::init.
