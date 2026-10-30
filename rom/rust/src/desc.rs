//! ROM 0x1FFFEF44-0x1FFFEFC6: the USB descriptors, and 0x1FFFEE40-0x1FFFEE53:
//! the memory-size tables GET_INFO and the address check use.

/// 0x1FFFEF44: EP0's packet size. A byte of its own just before the device
/// descriptor; usb_ep0 reads it on every packet.
pub static EP0_MAXP: u8 = 0x40;

/// 0x1FFFEF45: USB 2.0, class in the interface, EP0 64 bytes, 342d:dfa0,
/// bcdDevice 1.00, strings 1 and 2, no serial number, one configuration.
pub static DEVICE: [u8; 0x12] = [
    0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x2D, 0x34, 0xA0, 0xDF, 0x00, 0x01, 0x01, 0x02,
    0x00, 0x01,
];

/// 0x1FFFEF57: one interface, vendor class 0xFF/0xFF, bus powered, 100 mA,
/// bulk EP1 IN (0x81) and EP1 OUT (0x01) of 64 bytes. Not USB DFU.
pub static CONFIG: [u8; 0x20] = [
    0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 0x32, // configuration
    0x09, 0x04, 0x00, 0x00, 0x02, 0xFF, 0xFF, 0x00, 0x00, // interface 0
    0x07, 0x05, 0x81, 0x02, 0x40, 0x00, 0x00, // EP1 IN
    0x07, 0x05, 0x01, 0x02, 0x40, 0x00, 0x00, // EP1 OUT
];

/// 0x1FFFEF77: string 0, English (US).
pub static LANGS: [u8; 4] = [0x04, 0x03, 0x09, 0x04];

/// 0x1FFFEF7B: string 1.
pub static MANUFACTURER: [u8; 0x20] = utf16::<0x20>("Westberry Tech.");

/// 0x1FFFEF9B: string 2.
pub static PRODUCT: [u8; 0x2C] = utf16::<0x2C>("WB Device in DFU Mode");

/// A string descriptor from ASCII.
const fn utf16<const N: usize>(s: &str) -> [u8; N] {
    let s = s.as_bytes();
    assert!(N == 2 + 2 * s.len());
    let mut d = [0; N];
    d[0] = N as u8;
    d[1] = 0x03;
    let mut i = 0;
    while i < s.len() {
        d[2 + 2 * i] = s[i];
        i += 1;
    }
    d
}

/// 0x1FFFEE40: SRAM size in 4 KB units, by SYS_MEMSZ bits 4-5: 36, 28, 20,
/// 12 KB.
pub static RAM_4K: [u8; 4] = [0x09, 0x07, 0x05, 0x03];

/// 0x1FFFEE44: flash size in 4 KB units, by SYS_MEMSZ bits 0-3. 3 is
/// 256 KB, the WB32FQ95's.
pub static FLASH_4K: [u8; 16] = [
    0x80, 0x08, 0x60, 0x40, 0x20, 0x08, 0x18, 0x10, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x30,
];
