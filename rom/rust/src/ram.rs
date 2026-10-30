//! What the ROM keeps in SRAM, at the ROM's addresses (link.x checks them).
//! `DATA` is its .data, 0x20000000-0x20000017, copied from 0x1FFFEFE8 at
//! reset; `BSS` is 0x20000018-0x2000024B, zeroed. The stack is above them,
//! down from 0x20000450. Nothing else in SRAM is touched, so boot-guard's
//! magic word at 0x20006FFC and its records survive a trip through DFU.

use core::cell::UnsafeCell;
use core::ptr;

/// The ROM's initial SP (vector 0, and reloaded by the reset handler).
pub const STACK_TOP: u32 = 0x2000_0450;

/// One SRAM variable. Every access is volatile: the command loop polls
/// what the USB interrupt writes.
#[repr(transparent)]
pub struct V<T>(UnsafeCell<T>);

unsafe impl<T> Sync for V<T> {}

impl<T: Copy> V<T> {
    pub const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }
    #[inline(always)]
    pub fn get(&self) -> T {
        unsafe { self.0.get().read_volatile() }
    }
    #[inline(always)]
    pub fn set(&self, v: T) {
        unsafe { self.0.get().write_volatile(v) }
    }
    #[inline(always)]
    pub fn ptr(&self) -> *mut T {
        self.0.get()
    }
}

#[repr(C)]
pub struct Data {
    /// 0x20000000: a packet waits on EP1 OUT (usb::ep1_out sets it).
    pub rx_ready: V<u32>,
    /// 0x20000004: an EP1 IN packet is in flight (usb::ep1_in clears it).
    pub tx_busy: V<u32>,
    /// 0x20000008: what GET_STATUS, and GET_CONFIGURATION before
    /// configuration, answer with.
    pub status: V<u16>,
    /// 0x2000000A: the EP0 stage: 0 idle, 1 data IN, 2 data OUT, 3 stalled
    /// (until the stall is sent).
    pub ep0: V<u8>,
    _pad: u8,
    /// 0x2000000C: string descriptors 0-2. In .data rather than in the ROM,
    /// as a C compiler places a non-const array of pointers.
    pub strings: [*const u8; 3],
}

unsafe impl Sync for Data {}

#[unsafe(link_section = ".data.ram_data")]
pub static DATA: Data = Data {
    rx_ready: V::new(0),
    tx_busy: V::new(0),
    status: V::new(0),
    ep0: V::new(0),
    _pad: 0,
    strings: [
        crate::desc::LANGS.as_ptr(),
        crate::desc::MANUFACTURER.as_ptr(),
        crate::desc::PRODUCT.as_ptr(),
    ],
};

#[repr(C)]
pub struct Bss {
    /// 0x20000018: the command from the host (dfu.rs), at most 0x110 bytes.
    pub cmd: V<[u8; 0x110]>,
    /// 0x20000128: the reply.
    pub reply: V<[u8; 0x110]>,
    /// 0x20000238: control transfers and the device state.
    pub ctrl: Ctrl,
}

/// 0x14 bytes, zeroed by usb::init and on every bus reset.
#[repr(C)]
pub struct Ctrl {
    /// +0x00: the SETUP packet as read from the FIFO.
    pub setup: V<[u8; 8]>,
    /// +0x08: where the data stage reads from (or writes to).
    pub data: V<*const u8>,
    /// +0x0C: bytes left in the data stage.
    pub left: V<u16>,
    /// +0x0E: bytes done.
    pub done: V<u16>,
    /// +0x10: bConfigurationValue.
    pub config: V<u8>,
    /// +0x11: DEVICE_REMOTE_WAKEUP, as set by the host.
    pub remote_wakeup: V<u8>,
    /// +0x12: 0 default, 1 addressed, 2 configured, 3 suspended.
    pub state: V<u8>,
    /// +0x13: the state before the suspend.
    pub saved: V<u8>,
}

pub const CTRL_LEN: usize = 0x14;

impl Ctrl {
    #[inline(always)]
    fn byte(&self, i: usize) -> u8 {
        unsafe { (self.setup.ptr() as *const u8).add(i).read_volatile() }
    }
    #[inline(always)]
    fn half(&self, i: usize) -> u16 {
        unsafe {
            (self.setup.ptr() as *const u8)
                .add(i)
                .cast::<u16>()
                .read_volatile()
        }
    }
    pub fn request_type(&self) -> u8 {
        self.byte(0)
    }
    pub fn request(&self) -> u8 {
        self.byte(1)
    }
    pub fn value(&self) -> u16 {
        self.half(2)
    }
    pub fn value_lo(&self) -> u8 {
        self.byte(2)
    }
    pub fn value_hi(&self) -> u8 {
        self.byte(3)
    }
    pub fn index_lo(&self) -> u8 {
        self.byte(4)
    }
    pub fn length(&self) -> u16 {
        self.half(6)
    }
}

unsafe impl Sync for Bss {}

#[unsafe(link_section = ".bss.ram_bss")]
pub static BSS: Bss = Bss {
    cmd: V::new([0; 0x110]),
    reply: V::new([0; 0x110]),
    ctrl: Ctrl {
        setup: V::new([0; 8]),
        data: V::new(ptr::null()),
        left: V::new(0),
        done: V::new(0),
        config: V::new(0),
        remote_wakeup: V::new(0),
        state: V::new(0),
        saved: V::new(0),
    },
};

/// ROM 0x1FFFE0BC-0x1FFFE0CD: memset, and the zeroing form at 0x1FFFE0CA.
pub fn memset(p: *mut u8, n: usize, v: u8) {
    for i in 0..n {
        unsafe { p.add(i).write_volatile(v) }
    }
}
