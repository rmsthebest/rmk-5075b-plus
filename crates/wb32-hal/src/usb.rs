//! USB device (full speed) on the WB32's Mentor MUSB controller.
//!
//! The controller itself is driven by the `musb` crate (our `wb32fq95`
//! profile in `third_party/musb`). This module adds what is specific to the
//! chip, ported from ChibiOS-contrib:
//! - [`Driver::new`]: clocks, reset, FIFO clock and PHY (`wb32_usb_init` in
//!   `WB32FQ95xx/hal_lld.c`, with QMK's `WB32_USBPRE_DIV2`: 96 / 2 = 48 MHz),
//!   then the bus held disconnected (`wb32_usb_disconnect`).
//! - [`Bus::enable`]: after 50 ms disconnected, PA11/PA12 to the USB
//!   function, which attaches D+ (`wb32_usb_connect`). QMK's
//!   `init_usb_driver` waits the same 50 ms so a reset is seen by the host as
//!   a replug. The wait counts CPU cycles rather than using embassy-time, so
//!   USB (and with it the log) comes up even if the time base doesn't.
//! - After a bus reset: `POWER = SUSEN` and `INTRUSBE.SUSIE` again
//!   (`usb_lld_reset`). The reset clears both, and musb's lite profile only
//!   sets the interrupt enable once, so without this suspend goes unreported.
//! - Remote wakeup: RESUME for 2 ms (`usb_lld_wakeup_host`, QMK's
//!   `WB32_USB_HOST_WAKEUP_DURATION`).
//!
//! The pins and the `USB` interrupt belong to this module once
//! [`Driver::new`] has run.

use embassy_time::{Duration, Timer};
use embassy_usb_driver as driver;
use embassy_usb_driver::{EndpointAddress, EndpointAllocError, EndpointType, Event, Unsupported};
use musb::{In, MusbDriver, Out, UsbInstance};
use wb32_pac::{GPIOA, Interrupt, RCC, SFM, USB, interrupt, rcc, sfm};

use crate::gpio::{self, Mode, Port, Pull};

/// D- and D+ (PA11, PA12), alternate function 3.
const DM_DP: u32 = (1 << 11) | (1 << 12);
const DP: u32 = 1 << 12;
const AF_USB: u8 = 3;

/// How long D+ stays low before attaching (QMK `init_usb_driver`), in CPU
/// cycles. `asm::delay` takes at least this many.
const DISCONNECT_CYCLES: u32 = crate::rcc::HCLK_HZ / 1000 * 50;
/// How long remote wakeup drives resume signalling (QMK `mcuconf.h`).
/// RM2905025 asks for about 10 ms (2-15, pp. 266/285) and ChibiOS's default
/// is 10; 2 is the manual's minimum. Kept at QMK's value, which its builds for
/// this board ship with. If a host misses remote wakeups, try 10 first.
const WAKEUP_TIME: Duration = Duration::from_millis(2);

/// RCC USBPRE: SRCEN, then ratio 0 (/2) with the divider enabled.
const USBPRE_DIV2_RATIO: u8 = 0;

pub struct Driver<'d> {
    inner: MusbDriver<'d, UsbInstance>,
}

impl<'d> Driver<'d> {
    /// Brings up the USB block and holds the bus disconnected; embassy-usb
    /// attaches it from [`Bus::enable`]. `prio` is the USB interrupt priority
    /// (upper 4 bits used). Call after [`crate::rcc::init`].
    pub fn new(_dm: gpio::PA11, _dp: gpio::PA12, prio: u8) -> Self {
        disconnect();
        init_block();
        let inner = MusbDriver::new();
        unsafe {
            let mut nvic = cortex_m::Peripherals::steal().NVIC;
            nvic.set_priority(Interrupt::USB, prio);
            cortex_m::peripheral::NVIC::unpend(Interrupt::USB);
            cortex_m::peripheral::NVIC::unmask(Interrupt::USB);
        }
        Driver { inner }
    }
}

/// `wb32_usb_init`, register for register.
fn init_block() {
    RCC.ahbenr1().modify(|w| w.set_crcsfmen(true));
    RCC.ahbenr1().modify(|w| w.set_usben(true));
    RCC.ahbrstr1().modify(|w| w.set_usbrst(true));
    RCC.ahbrstr1().modify(|w| w.set_usbrst(false));
    RCC.usbfifoclksrc().write_value(rcc::regs::Usbfifoclksrc(1)); // USBCLK
    RCC.usbfifoclkenr().write_value(rcc::regs::Usbfifoclkenr(1)); // CLKEN
    SFM.usbpcon().write_value(sfm::regs::Usbpcon(0x02));
    RCC.usbclkenr().write_value(rcc::regs::Usbclkenr(1)); // CLKEN
    RCC.usbpre().write(|w| w.set_srcen(true));
    RCC.usbpre().modify(|w| w.set_ratio(USBPRE_DIV2_RATIO));
    RCC.usbpre().modify(|w| w.set_diven(true));
}

/// `wb32_usb_disconnect`: PA11/PA12 inputs, then D+ driven low.
fn disconnect() {
    gpio::set_mode(Port::A, DM_DP, Mode::input(Pull::None));
    GPIOA.bsrr().write_value(wb32_pac::gpio::regs::Bsrr(DP << 16));
    gpio::set_mode(Port::A, DP, Mode::output_open_drain());
}

/// `wb32_usb_connect`: PA11/PA12 to the USB function at full drive, then
/// suspend detection and the bus interrupts.
fn connect() {
    gpio::set_mode(Port::A, DM_DP, Mode { current: 3, ..Mode::alternate(AF_USB) });
    USB.power().write(|w| w.set_susen(true));
    USB.intrusbe().write(|w| {
        w.set_rstie(true);
        w.set_rsuie(true);
        w.set_susie(true);
    });
}

impl<'d> driver::Driver<'d> for Driver<'d> {
    type EndpointOut = musb::Endpoint<'d, UsbInstance, Out>;
    type EndpointIn = musb::Endpoint<'d, UsbInstance, In>;
    type ControlPipe = musb::ControlPipe<'d, UsbInstance>;
    type Bus = Bus<'d>;

    fn alloc_endpoint_in(
        &mut self,
        ep_type: EndpointType,
        ep_addr: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        self.inner.alloc_endpoint(ep_type, ep_addr, max_packet_size, interval_ms)
    }

    fn alloc_endpoint_out(
        &mut self,
        ep_type: EndpointType,
        ep_addr: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        self.inner.alloc_endpoint(ep_type, ep_addr, max_packet_size, interval_ms)
    }

    fn start(self, control_max_packet_size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (inner, pipe) = self.inner.start(control_max_packet_size);
        (Bus { inner }, pipe)
    }
}

pub struct Bus<'d> {
    inner: musb::Bus<'d, UsbInstance>,
}

impl<'d> driver::Bus for Bus<'d> {
    async fn enable(&mut self) {
        self.inner.enable().await;
        // Blocks the executor once, at start-up.
        cortex_m::asm::delay(DISCONNECT_CYCLES);
        connect();
    }

    async fn disable(&mut self) {
        self.inner.disable().await;
        disconnect();
    }

    async fn poll(&mut self) -> Event {
        let event = self.inner.poll().await;
        if let Event::Reset = event {
            USB.power().write(|w| w.set_susen(true));
            USB.intrusbe().modify(|w| w.set_susie(true));
        }
        event
    }

    fn endpoint_set_enabled(&mut self, ep_addr: EndpointAddress, enabled: bool) {
        self.inner.endpoint_set_enabled(ep_addr, enabled)
    }

    fn endpoint_set_stalled(&mut self, ep_addr: EndpointAddress, stalled: bool) {
        self.inner.endpoint_set_stalled(ep_addr, stalled)
    }

    fn endpoint_is_stalled(&mut self, ep_addr: EndpointAddress) -> bool {
        self.inner.endpoint_is_stalled(ep_addr)
    }

    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        USB.power().modify(|w| w.set_resume(true));
        Timer::after(WAKEUP_TIME).await;
        USB.power().modify(|w| w.set_resume(false));
        Ok(())
    }
}

#[interrupt]
fn USB() {
    unsafe { musb::on_interrupt::<UsbInstance>() }
}

