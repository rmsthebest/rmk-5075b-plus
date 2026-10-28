//! EP0: control transfers and the standard requests. ROM
//! 0x1FFFE1A0-0x1FFFE3D7, 0x1FFFE4DC-0x1FFFE623 and 0x1FFFE718-0x1FFFE753.
//! Only standard requests are answered; class and vendor requests stall.

use crate::desc;
use crate::mmio::*;
use crate::ram::{BSS, DATA};
use crate::usb;
use core::ptr;
use wb32_pac::USB;

// CSR0 bits.
const OUT_PKT_RDY: u8 = 0x01;
const IN_PKT_RDY: u8 = 0x02;
const SENT_STALL: u8 = 0x04;
const DATA_END: u8 = 0x08;
const SETUP_END: u8 = 0x10;
const SEND_STALL: u8 = 0x20;
const SERVICED_OUT_PKT_RDY: u8 = 0x40;
const SERVICED_SETUP_END: u8 = 0x80;

/// What a request handler asks of the control transfer.
const STALL: u8 = 0;
const DATA_IN: u8 = 1;
const DATA_OUT: u8 = 2;
const NO_DATA: u8 = 3;

/// ROM 0x1FFFE1A0-0x1FFFE31F: the EP0 interrupt.
pub fn ep0() {
    let c = &BSS.ctrl;
    let stage = &DATA.ep0;
    w8(USB.index(), 0);
    let csr = r8(USB.csr0());
    if csr & SENT_STALL != 0 {
        w8(USB.csr0(), 0);
        stage.set(0);
    }
    if csr & SETUP_END != 0 {
        w8(USB.csr0(), SERVICED_SETUP_END);
        stage.set(0);
    }

    // Stage 3 (stalled) waits here for SentStall.
    if stage.get() == 0 && csr & OUT_PKT_RDY != 0 {
        let mut r = STALL;
        usb::fifo_read(0, c.setup.ptr() as *mut u8, 8);
        c.data.set(ptr::null());
        c.left.set(c.length());
        c.done.set(0);
        if c.request_type() & 0x60 == 0 {
            r = setup();
        }
        w8(USB.index(), 0);
        match r {
            DATA_IN => {
                w8(USB.csr0(), SERVICED_OUT_PKT_RDY);
                stage.set(1);
            }
            DATA_OUT => {
                w8(USB.csr0(), SERVICED_OUT_PKT_RDY);
                stage.set(2);
            }
            NO_DATA => {
                w8(USB.csr0(), SERVICED_OUT_PKT_RDY | DATA_END);
                stage.set(0);
            }
            _ => {
                w8(USB.csr0(), SERVICED_OUT_PKT_RDY | SEND_STALL);
                stage.set(3);
            }
        }
    }

    // Data IN: the next packet once the FIFO is free. The test on the
    // current CSR0 skips it only when OutPktRdy and SetupEnd are both set
    // (the ROM computes csr & -(csr & 1) and tests bit 4).
    if stage.get() == 1 && csr & IN_PKT_RDY == 0 {
        let now = r8(USB.csr0());
        if !(now & OUT_PKT_RDY != 0 && now & SETUP_END != 0) {
            let maxp = desc::EP0_MAXP as u16;
            let mut v = IN_PKT_RDY;
            if c.left.get() < maxp {
                usb::fifo_write(0, c.data.get(), c.left.get());
                c.done.set(c.done.get() + c.left.get());
                c.left.set(0);
                v = IN_PKT_RDY | DATA_END;
                stage.set(0);
            } else {
                usb::fifo_write(0, c.data.get(), maxp);
                c.data.set(c.data.get().wrapping_add(maxp as usize));
                c.left.set(c.left.get() - maxp);
                c.done.set(c.done.get() + maxp);
            }
            if c.done.get() == c.length() {
                v |= DATA_END;
                stage.set(0);
            }
            w8(USB.csr0(), v);
        }
    }

    // Data OUT. Nothing returns DATA_OUT, so this is never reached; the last
    // packet's 0x68 would also set SendStall.
    if stage.get() == 2 && r8(USB.csr0()) & OUT_PKT_RDY != 0 {
        let maxp = desc::EP0_MAXP as u16;
        let mut v = SERVICED_OUT_PKT_RDY;
        if c.left.get() < maxp {
            usb::fifo_read(0, c.data.get() as *mut u8, c.left.get());
            c.done.set(c.done.get() + c.left.get());
            c.left.set(0);
        } else {
            usb::fifo_read(0, c.data.get() as *mut u8, maxp);
            c.data.set(c.data.get().wrapping_add(maxp as usize));
            c.left.set(c.left.get() - maxp);
            c.done.set(c.done.get() + maxp);
        }
        if c.left.get() == 0 {
            v = SERVICED_OUT_PKT_RDY | SEND_STALL | DATA_END;
            stage.set(3);
            w8(USB.index(), 0);
        }
        w8(USB.csr0(), v);
    }
}

/// ROM 0x1FFFE330-0x1FFFE3CB: a standard request, by bRequest (a table
/// branch over 0-9). SET_DESCRIPTOR, GET_INTERFACE, SET_INTERFACE and
/// SYNCH_FRAME stall.
fn setup() -> u8 {
    let c = &BSS.ctrl;
    match c.request() {
        // GET_STATUS. The direction bit isn't checked.
        0 => {
            if c.length() == 2 && get_status() {
                DATA_IN
            } else {
                STALL
            }
        }
        // CLEAR_FEATURE, SET_FEATURE.
        1 | 3 => {
            if feature(c.request() == 3) {
                NO_DATA
            } else {
                STALL
            }
        }
        // SET_ADDRESS. FADDR is written at once, before the status stage;
        // refused once configured.
        5 => {
            if c.request_type() != 0 {
                return STALL;
            }
            let addr = c.value_lo() & 0x7F;
            if c.state.get() == 2 {
                return STALL;
            }
            w8(USB.faddr(), addr);
            c.state.set(if addr != 0 { 1 } else { 0 });
            NO_DATA
        }
        // GET_DESCRIPTOR.
        6 => {
            if c.request_type() == 0x80 && get_descriptor() {
                DATA_IN
            } else {
                STALL
            }
        }
        // GET_CONFIGURATION: 0 from `status` until configured.
        8 => {
            if c.request_type() != 0x80 || c.length() != 1 {
                return STALL;
            }
            match c.state.get() {
                0 | 1 => {
                    DATA.status.set(0);
                    c.data.set(DATA.status.ptr() as *const u8);
                }
                2 => c.data.set(c.config.ptr() as *const u8),
                _ => return STALL,
            }
            DATA_IN
        }
        // SET_CONFIGURATION.
        9 => {
            if c.request_type() == 0 && set_configuration() {
                NO_DATA
            } else {
                STALL
            }
        }
        _ => STALL,
    }
}

/// ROM 0x1FFFE4DC-0x1FFFE523: device, configuration or string; the reply
/// is cut to the descriptor's length.
fn get_descriptor() -> bool {
    let c = &BSS.ctrl;
    // The ROM leaves this uninitialised; it's only read after a descriptor
    // was found, which sets it.
    let mut len = 0;
    let p = match c.value_hi() {
        1 => device_descriptor(&mut len),
        2 => config_descriptor(c.value_lo(), &mut len),
        3 => string_descriptor(c.value_lo(), &mut len),
        _ => return false,
    };
    c.data.set(p);
    if p.is_null() {
        return false;
    }
    if len < c.left.get() {
        c.left.set(len);
    }
    true
}

/// ROM 0x1FFFE72C-0x1FFFE733.
fn device_descriptor(len: &mut u16) -> *const u8 {
    *len = 0x12;
    desc::DEVICE.as_ptr()
}

/// ROM 0x1FFFE718-0x1FFFE727: only index 0.
fn config_descriptor(index: u8, len: &mut u16) -> *const u8 {
    if index != 0 {
        return ptr::null();
    }
    *len = 0x20;
    desc::CONFIG.as_ptr()
}

/// ROM 0x1FFFE738-0x1FFFE74F: strings 0-2 through the table in .data; the
/// length is the descriptor's first byte. The language ID isn't checked.
fn string_descriptor(index: u8, len: &mut u16) -> *const u8 {
    if index < 3 {
        let p = DATA.strings[index as usize];
        *len = unsafe { p.read() } as u16;
        return p;
    }
    ptr::null()
}

/// ROM 0x1FFFE528-0x1FFFE573: GET_STATUS for the device, an interface or an
/// endpoint.
fn get_status() -> bool {
    let c = &BSS.ctrl;
    let status = &DATA.status;
    status.set(0);
    match c.request_type() & 0x1F {
        0 => {
            // Self-powered from bmAttributes, found by looking up the
            // configuration descriptor with the current configuration as
            // the descriptor index. That only works unconfigured (index 0):
            // once configured the lookup fails and the ROM reads byte 7 of a
            // null pointer, address 0x00000007 (the boot alias). Kept.
            let mut len = 0;
            let config = config_descriptor(c.config.get(), &mut len);
            if unsafe { config.wrapping_add(7).read_volatile() } & 0x40 != 0 {
                status.set(1);
            }
            if c.remote_wakeup.get() != 0 {
                status.set(status.get() | 2);
            }
        }
        1 => {}
        2 => status.set(usb::halted(c.index_lo()) as u16),
        _ => return false,
    }
    c.data.set(status.ptr() as *const u8);
    true
}

/// ROM 0x1FFFE57C-0x1FFFE5BF: CLEAR_FEATURE / SET_FEATURE:
/// DEVICE_REMOTE_WAKEUP, or ENDPOINT_HALT on any endpoint but 0. Interface
/// recipients stall.
fn feature(set: bool) -> bool {
    let c = &BSS.ctrl;
    match c.request_type() & 0x1F {
        0 => {
            if c.value() != 1 {
                return false;
            }
            c.remote_wakeup.set(set as u8);
            true
        }
        2 => {
            let ep = c.index_lo();
            if c.value() != 0 || ep & 0x0F == 0 {
                return false;
            }
            if set {
                usb::set_halt(ep);
            } else {
                usb::clear_halt(ep);
            }
            true
        }
        _ => false,
    }
}

/// ROM 0x1FFFE5C4-0x1FFFE61F: addressed -> configured, a change of
/// configuration, or back to addressed with 0.
fn set_configuration() -> bool {
    let c = &BSS.ctrl;
    let config = c.value_lo();
    match c.state.get() {
        1 => {
            if config != 0 {
                if !usb::configure(config) {
                    return false;
                }
                c.config.set(config);
                c.state.set(2);
            }
        }
        2 => {
            if config == 0 {
                c.config.set(0);
                c.state.set(1);
                usb::unconfigure();
            } else if c.config.get() != config {
                usb::unconfigure();
                if !usb::configure(config) {
                    return false;
                }
                c.config.set(config);
            }
        }
        _ => {
            usb::unconfigure();
            return false;
        }
    }
    true
}
