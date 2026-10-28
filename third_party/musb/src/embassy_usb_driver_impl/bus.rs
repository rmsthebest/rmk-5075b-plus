use super::*;

use crate::alloc_endpoint::EndpointConfig;
use crate::common_impl;
use crate::info::ENDPOINTS;

/// USB bus.
pub struct Bus<'d, T: MusbInstance> {
    pub(super) phantom: PhantomData<&'d mut T>,
    pub(super) ep_confs: [EndpointConfig; ENDPOINTS.len()],
    pub(super) inited: bool,
}

impl<'d, T: MusbInstance> Bus<'d, T> {
    fn init(&self) {
        #[cfg(not(feature="_lite"))]
        trace!("musb/bus init: DEVCTL: {:b}", T::regs().devctl().read().0);
        common_impl::bus_init::<T>();
    }
}

impl<'d, T: MusbInstance> driver::Bus for Bus<'d, T> {
    async fn poll(&mut self) -> Event {
        poll_fn(move |cx| {
            BUS_WAKER.register(cx.waker());

            // TODO: implement VBUS detection.
            if !self.inited {
                self.init();
                self.inited = true;
                return Poll::Ready(Event::PowerDetected);
            }

            if IRQ_RESET.load(Ordering::Acquire) {
                IRQ_RESET.store(false, Ordering::Relaxed);

                // No EP0 ack or FIFO flush here: the reset itself flushes
                // every FIFO (RM2905025 §21.5, as ChibiOS's usb_lld_reset
                // assumes), and by the time this runs the host's first SETUP
                // may already be in EP0's FIFO.

                // A bus reset disables every endpoint but EP0 until the host
                // configures the device again.
                EP_TX_ENABLED.store(EP_TX_ENABLED.load(Ordering::Acquire) & 1, Ordering::Release);
                EP_RX_ENABLED.store(EP_RX_ENABLED.load(Ordering::Acquire) & 1, Ordering::Release);

                trace!("musb/poll: reset");

                for w in &EP_TX_WAKERS {
                    w.wake()
                }
                for w in &EP_RX_WAKERS {
                    w.wake()
                }

                return Poll::Ready(Event::Reset);
            }

            if IRQ_SUSPEND.load(Ordering::Acquire) {
                IRQ_SUSPEND.store(false, Ordering::Relaxed);
                return Poll::Ready(Event::Suspend);
            }

            // After Suspend: if both were latched by one interrupt, the bus
            // was suspended first.
            if IRQ_RESUME.load(Ordering::Acquire) {
                IRQ_RESUME.store(false, Ordering::Relaxed);
                return Poll::Ready(Event::Resume);
            }

            Poll::Pending
        })
        .await
    }

    fn endpoint_set_stalled(&mut self, ep_addr: EndpointAddress, stalled: bool) {
        let ep_index = ep_addr.index();
        // The index comes from the host (SET/CLEAR_FEATURE(ENDPOINT_HALT)'s
        // wIndex): one this chip doesn't have is ignored, not a panic.
        if ep_index >= ENDPOINTS.len() {
            return;
        }
        match ep_addr.direction() {
            Direction::In => {
                common_impl::ep_tx_stall::<T>(ep_index as _, stalled);
                EP_TX_WAKERS[ep_index].wake();
            }
            Direction::Out => {
                common_impl::ep_rx_stall::<T>(ep_index as _, stalled);
                EP_RX_WAKERS[ep_index].wake();
            }
        }
    }

    fn endpoint_is_stalled(&mut self, ep_addr: EndpointAddress) -> bool {
        if ep_addr.index() >= ENDPOINTS.len() {
            return false;
        }
        match ep_addr.direction() {
            Direction::In => common_impl::ep_tx_is_stalled::<T>(ep_addr.index() as _),
            Direction::Out => common_impl::ep_rx_is_stalled::<T>(ep_addr.index() as _),
        }
    }

    fn endpoint_set_enabled(&mut self, ep_addr: EndpointAddress, enabled: bool) {
        trace!("musb/bus/set_enabled: {:x} {}", ep_addr, enabled);
        let ep_index = ep_addr.index();

        if enabled {
            T::regs().index().write(|w| w.set_index(ep_index as u8));
            match ep_addr.direction() {
                Direction::Out => {
                    common_impl::ep_rx_enable::<T>(ep_index as _, &self.ep_confs[ep_index]);

                    let flags = EP_RX_ENABLED.load(Ordering::Acquire) | (1 << ep_index) as u16;
                    EP_RX_ENABLED.store(flags, Ordering::Release);
                    // Wake for `Endpoint::wait_enabled()`
                    EP_RX_WAKERS[ep_index].wake();
                }
                Direction::In => {
                    common_impl::ep_tx_enable::<T>(ep_index as _, &self.ep_confs[ep_index]);

                    let flags = EP_TX_ENABLED.load(Ordering::Acquire) | (1 << ep_index) as u16;
                    EP_TX_ENABLED.store(flags, Ordering::Release);
                    // Wake for `Endpoint::wait_enabled()`
                    EP_TX_WAKERS[ep_index].wake();
                }
            }
        } else {
            // py32 official CherryUsb port does nothing when disable an endpoint
            match ep_addr.direction() {
                Direction::Out => {
                    let flags = EP_RX_ENABLED.load(Ordering::Acquire) & !((1 << ep_index) as u16);
                    EP_RX_ENABLED.store(flags, Ordering::Release);
                    // A pending read returns `Disabled`.
                    EP_RX_WAKERS[ep_index].wake();
                }
                Direction::In => {
                    let flags = EP_TX_ENABLED.load(Ordering::Acquire) & !((1 << ep_index) as u16);
                    EP_TX_ENABLED.store(flags, Ordering::Release);
                    EP_TX_WAKERS[ep_index].wake();
                }
            }
        }
    }

    async fn enable(&mut self) {
        T::regs().faddr().write(|w| w.set_func_addr(0));

        #[cfg(not(feature = "_lite"))]
        T::regs().power().modify(|w| {
            w.set_soft_conn(true);
        });

        // T::regs().devctl().write(|w| {
        //     w.set_session(true);
        // });
        // self.endpoint_set_enabled(EndpointAddress::from_parts(0, Direction::In), true);
        // self.endpoint_set_enabled(EndpointAddress::from_parts(0, Direction::Out), true);
    }
    async fn disable(&mut self) {
        #[cfg(not(feature = "_lite"))]
        T::regs().power().modify(|w| {
            w.set_soft_conn(false);
        });
    }

    async fn remote_wakeup(&mut self) -> Result<(), Unsupported> {
        Err(Unsupported)
    }
}
