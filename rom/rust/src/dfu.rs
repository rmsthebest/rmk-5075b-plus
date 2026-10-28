//! The command loop on bulk EP1: ROM 0x1FFFEAF2-0x1FFFEC8F, with the
//! address and protection checks at 0x1FFFE8CC and 0x1FFFE928. A command is
//! `<cmd> <sub> 00 00 <arg0 LE> <arg1 LE> <data>`; the reply is `<cmd>
//! <status> [data]`. notes.md "Commands" has the table.

use crate::clock::feed;
use crate::mmio::*;
use crate::ram::{BSS, memset};
use crate::{boot, desc, flash, usb};
use wb32_pac::{FMC, SYS};

const GET_INFO: u8 = 0x01;
const ERASE: u8 = 0x5E;
const WRITE_PAGE: u8 = 0x6A;
const READ: u8 = 0x72;
const RESET: u8 = 0x83;
const GO: u8 = 0x9B;

const OK: u8 = 0x00;
pub const ARG_ERROR: u8 = 0x01;
const ERASE_FAILED: u8 = 0x11;
const WRITE_FAILED: u8 = 0x12;
const PROTECTED: u8 = 0x1A;
const INVALID: u8 = 0xFF;

/// Where an address is: main flash, information blocks 4-7, or neither.
const NOWHERE: u8 = 0;
pub const FLASH: u8 = 1;
const INFO_4_7: u8 = 2;

fn cmd(i: usize) -> u8 {
    unsafe { (BSS.cmd.ptr() as *const u8).add(i).read() }
}

fn arg(i: usize) -> u32 {
    unsafe { (BSS.cmd.ptr() as *const u8).add(4 + 4 * i).cast::<u32>().read() }
}

fn reply(i: usize, v: u8) {
    unsafe { (BSS.reply.ptr() as *mut u8).add(i).write(v) }
}

fn reply32(i: usize, v: u32) {
    unsafe { (BSS.reply.ptr() as *mut u8).add(i).cast::<u32>().write(v) }
}

/// ROM 0x1FFFEAFE-0x1FFFEC8F (RESET and GO in boot.rs).
pub fn run() -> ! {
    loop {
        memset(BSS.cmd.ptr() as *mut u8, 0x110, 0);
        if usb::recv() == 0 {
            continue;
        }
        let op = cmd(0);
        reply(0, op);
        reply(1, OK);
        let status = match op {
            GET_INFO => {
                if cmd(1) == 0 {
                    usb::send(get_info());
                    continue;
                }
                ARG_ERROR
            }
            ERASE => erase(),
            WRITE_PAGE => write_page(),
            READ => match read() {
                Ok(n) => {
                    usb::send(n + 2);
                    continue;
                }
                Err(s) => s,
            },
            RESET => boot::reset(arg(0)),
            GO => boot::go(arg(0), arg(1)),
            _ => INVALID,
        };
        if status != OK {
            reply(1, status);
        }
        usb::send(2);
    }
}

/// ROM 0x1FFFEB42-0x1FFFEB77: `0d <version> <SYS_ID> <flash bytes> <SRAM
/// bytes>`. The ROM's chip answers `01 00 0d 04 80 e9 50 3a 00 00 04 00 00
/// 90 00 00`. What 0x0D stands for isn't known.
fn get_info() -> usize {
    reply(2, 0x0D);
    // The bootloader version, the low byte of vector 7.
    reply(3, unsafe { (0x1FFF_E01C as *const u8).read_volatile() });
    reply32(4, r32(SYS.id()));
    let memsz = r32(SYS.memsz());
    reply32(8, (desc::FLASH_4K[(memsz & 0xF) as usize] as u32) << 12);
    reply32(12, (desc::RAM_4K[((memsz >> 4) & 3) as usize] as u32) << 12);
    16
}

/// ROM 0x1FFFEB7E-0x1FFFEBDA: sub 0x00 erases arg1 pages of 256 bytes from
/// arg0 (rounded down to a page); both ends must be in main flash. Sub 0xF0
/// erases the whole chip. Refused outright while read-protected.
fn erase() -> u8 {
    if protected() {
        return PROTECTED;
    }
    match cmd(1) {
        0x00 => {
            let addr = arg(0) & !0xFF;
            let pages = arg(1);
            if region(addr) != FLASH || region(addr.wrapping_add(pages << 8).wrapping_sub(1)) != FLASH {
                return ARG_ERROR;
            }
            for i in 0..pages {
                feed();
                if flash::op(addr + (i << 8), flash::ERASE_PAGE) {
                    return ERASE_FAILED;
                }
            }
            OK
        }
        0xF0 => {
            if flash::op(0, flash::CHIP_ERASE) {
                return ERASE_FAILED;
            }
            OK
        }
        _ => ARG_ERROR,
    }
}

/// ROM 0x1FFFEBDC-0x1FFFEC4C: arg1 bytes (at most 0x100, whole words) from
/// the command into the page at arg0. A page of information blocks 4-7 is
/// erased first; a flash page has to be erased already. Only the start
/// address is checked. Protection stops flash writes but not information
/// block writes, which is how read protection gets lifted (notes.md
/// "Read protection").
fn write_page() -> u8 {
    let addr = arg(0) & !0xFF;
    let n = arg(1);
    let r = region(addr);
    if protected() && r == FLASH {
        return PROTECTED;
    }
    if r == NOWHERE || n > 0x100 {
        return ARG_ERROR;
    }
    if r == INFO_4_7 {
        flash::op(addr, flash::ERASE_PAGE);
    }
    flash::op(0, flash::CLEAR_LATCH);
    for i in 0..(n >> 2) as usize {
        w32(FMC.buf(i), unsafe { (BSS.cmd.ptr() as *const u8).add(12 + 4 * i).cast::<u32>().read() });
    }
    if flash::op(addr, flash::PROGRAM) {
        return WRITE_FAILED;
    }
    OK
}

/// ROM 0x1FFFEC56-0x1FFFEC8F: arg1 bytes (16 bits, at most 0x100) from arg0
/// into the reply. Both ends must be in flash or information blocks 4-7,
/// not necessarily the same one. Refused while read-protected.
fn read() -> Result<usize, u8> {
    let addr = arg(0);
    let n = arg(1) as u16 as u32;
    if protected() {
        return Err(PROTECTED);
    }
    if n > 0x100 || region(addr) == NOWHERE || region(addr.wrapping_add(n).wrapping_sub(1)) == NOWHERE {
        return Err(ARG_ERROR);
    }
    for i in 0..n as usize {
        reply(2 + i, unsafe { (addr as *const u8).add(i).read_volatile() });
    }
    Ok(n as usize)
}

/// ROM 0x1FFFE8CC-0x1FFFE8F7: main flash is 0x08000000 and the size from
/// SYS_MEMSZ; information blocks 4-7 are 0x1FFFF400-0x1FFFF7FF.
pub fn region(addr: u32) -> u8 {
    let size = (desc::FLASH_4K[(r32(SYS.memsz()) & 0xF) as usize] as u32) << 12;
    if addr.wrapping_sub(0x0800_0000) < size {
        return FLASH;
    }
    if addr.wrapping_sub(0x1FFF_F400) < 0x400 {
        return INFO_4_7;
    }
    NOWHERE
}

/// ROM 0x1FFFE928-0x1FFFE93B: read-protected unless the low nibble of
/// 0x1FFFF410 (information block 4) is 0xC.
fn protected() -> bool {
    r32(reg(INFO + 0x410)) & 0x0F != 0x0C
}
