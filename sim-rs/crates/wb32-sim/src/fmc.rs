//! The flash controller (FMC) at 0x4001_7800, +0x200, and SYS at
//! 0x4001_6400, as the ROM drives them: ports of Renode's rom/sim/peripherals/
//! fmc.py and sys.py (from rom/notes.md "Flash erase and program" and RM 3.3).
//!
//! FMC:
//!   0x00 CON   an op is <op bits 4:0, written with 0x7F5F0D40> then, after
//!              both keys, CON = 0x800080 (start); bit 7 reads as busy for
//!              the op's time (`op_ticks`, the datasheet's tPROG 2.1 ms,
//!              tERASE 6.4 ms, tME 25.6 ms; clearing the latch takes none).
//!              0x5F0000 clears the op and locks again.
//!   0x08 STAT  bit 2: the last op failed (locked, or an address no op may
//!              touch)
//!   0x0C KEY   0x5188DA08 then 0x12586590 unlocks one op
//!   0x10 ADDR  the page (256 bytes; low bits ignored)
//!   0x100-0x1FF the page latch the ROM fills before a program
//! Ops: 0x04 clear the latch, 0x08 erase the page, 0x0C program the latch into
//! it, 0x1B erase the whole flash. Pages: main flash 0x0800_0000-0x0803_FFFF,
//! and information blocks 4-7 (0x1FFF_F400-0x1FFF_F7FF), the only ones the
//! ROM's address check allows. Erased bytes are 0x00 (BOARD_FACTS.md).
//! The chip does the memory writes an op asks for (`Fmc::write` returns them).
//!
//! SYS (read-only identification for the ROM's GET_INFO):
//!   0x00 SYS_ID 0x3A50E980, what wb32-dfu-updater_cli prints for this chip
//!   0x04 MEMSZ  0x03: an assumption (the ROM's tables then give 256 KB /
//!               36 KB, as the tool reports); the board's value is unread
//!   0x34 BTSR   2: booted from user flash (the ROM is entered by a jump)

use crate::chip::{FLASH_BASE, FLASH_SIZE};
use cortex_m3_emu::TICK_HZ;

const KEY1: u32 = 0x5188_DA08;
const KEY2: u32 = 0x1258_6590;
const INFO_LO: u32 = 0x1FFF_F400;
const INFO_HI: u32 = 0x1FFF_F800;
const US: u64 = TICK_HZ / 1_000_000;

/// The current an op draws while busy (DS1905020 table 5.2-10: IDDPROG,
/// IDDERASE, maximums; the mass erase taken as an erase).
pub fn op_ua(op: u8) -> f64 {
    match op {
        0x0C => crate::power::PROGRAM_UA,
        0x08 | 0x1B => crate::power::ERASE_UA,
        _ => 0.0,
    }
}

/// How long an op keeps the flash busy (DS1905020 table "Flash memory
/// characteristics": tPROG, tERASE, tME).
pub fn op_ticks(op: u8) -> u64 {
    match op {
        0x0C => 2_100 * US,
        0x08 => 6_400 * US,
        0x1B => 25_600 * US,
        _ => 0,
    }
}

#[derive(Clone, Debug)]
pub struct Fmc {
    con: u32,
    stat: u32,
    addr: u32,
    keys: Vec<u32>,
    op: u32,
    latch: [u32; 64],
    /// When the op under way started and ends, and the current it draws.
    pub busy_from: u64,
    pub busy_until: u64,
    pub busy_ua: f64,
    /// CON reads that found it busy.
    pub busy_reads: u32,
    /// What ran: (op, page address).
    pub ops: Vec<(u8, u32)>,
    /// What was refused, as fmc.py's "FMC ERROR" lines.
    pub errors: Vec<String>,
}

impl Default for Fmc {
    fn default() -> Self {
        Fmc {
            con: 0,
            stat: 0,
            addr: 0,
            keys: Vec::new(),
            op: 0,
            latch: [0; 64],
            busy_from: 0,
            busy_until: 0,
            busy_ua: 0.0,
            busy_reads: 0,
            ops: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// Memory an op writes: words from an address.
pub struct Fill {
    pub addr: u32,
    pub words: Vec<u32>,
}

fn page_ok(a: u32) -> bool {
    (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&a) || (INFO_LO..INFO_HI).contains(&a)
}

impl Fmc {
    pub fn busy(&self, now: u64) -> bool {
        now < self.busy_until
    }

    pub fn read(&mut self, off: u32, now: u64) -> u32 {
        match off {
            0x00 if self.busy(now) => {
                self.busy_reads += 1;
                self.con | 0x80
            }
            0x00 => self.con & !0x80,
            0x08 => self.stat,
            0x10 => self.addr,
            0x100..0x200 => self.latch[((off - 0x100) / 4) as usize],
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64) -> Option<Fill> {
        let mut fill = None;
        match off {
            0x00 => {
                if v & 0x80_0000 != 0 && v & 0x80 != 0 {
                    fill = self.run(self.op as u8);
                    if fill.is_some() {
                        self.busy_from = now;
                        self.busy_until = now + op_ticks(self.op as u8);
                        self.busy_ua = op_ua(self.op as u8);
                    }
                } else if v == 0x5F_0000 {
                    self.op = 0;
                } else {
                    self.op = v & 0x1F;
                }
                self.con = v;
            }
            0x0C => self.keys.push(v),
            0x10 => self.addr = v,
            0x100..0x200 => self.latch[((off - 0x100) / 4) as usize] = v,
            _ => self.errors.push(format!(
                "FMC write to +0x{off:02x} = 0x{v:08x} not modelled"
            )),
        }
        fill
    }

    fn run(&mut self, op: u8) -> Option<Fill> {
        let a = self.addr & !0xFF;
        let unlocked = self.keys.ends_with(&[KEY1, KEY2]);
        self.keys.clear();
        if !unlocked {
            self.stat |= 4;
            self.errors
                .push(format!("FMC ERROR: op 0x{op:02x} without the keys"));
            return None;
        }
        let fill = match op {
            0x04 => {
                self.latch = [0; 64];
                None
            }
            0x1B => Some(Fill {
                addr: FLASH_BASE,
                words: vec![0; FLASH_SIZE / 4],
            }),
            0x08 | 0x0C if page_ok(a) => Some(Fill {
                addr: a,
                words: if op == 0x08 {
                    vec![0; 64]
                } else {
                    self.latch.to_vec()
                },
            }),
            _ => {
                self.stat |= 4;
                self.errors
                    .push(format!("FMC ERROR: op 0x{op:02x} at 0x{a:08x}"));
                return None;
            }
        };
        self.stat &= !4;
        self.ops.push((op, a));
        fill
    }
}

pub fn sys_read(off: u32) -> u32 {
    match off {
        0x00 => 0x3A50_E980,
        0x04 => 0x03,
        0x34 => 0x02,
        _ => 0,
    }
}
