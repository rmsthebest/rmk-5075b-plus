//! The board's SPI NOR flash on QSPI, chip select C12 (active low). A port
//! of the flash half of ../../sim/peripherals/qspi.py.
//!
//! The part number isn't known (the case stays shut), so the JEDEC ID and
//! size are QMK's default part's, the MX25L4006E (C2 20 13, 512 KB); the
//! firmware must read the capacity byte rather than assume it. Commands: 9F
//! RDID, 05 RDSR, 06 WREN, 04 WRDI, 03 READ, 0B FAST_READ, 02 PP (wraps
//! inside its 256-byte page), 20 SE (4 KB), D8 BE (64 KB), 60/C7 CE, B9 DP,
//! AB RDP/RES. PP/SE/BE/CE need WEL (set by WREN, cleared when the command
//! completes) and set WIP for BUSY_POLLS RDSR reads. Programming ANDs bits
//! in, as NOR does. A command ends at a rising edge of the chip select,
//! which (as in qspi.py) is noticed at the next byte. Every erase, program
//! and read goes to `log` ("SE 0x010000", "PP 0x010000 n=256").
//!
//! The array survives a system reset (`reset` keeps it), as the chip keeps
//! its data across a keyboard reset.

use wb32_sim::gpio::{B, C, Gpio, Pin};
use wb32_sim::ssi::{SsiDevice, SsiLine};

pub const SIZE: usize = 512 * 1024;
pub const JEDEC_ID: [u8; 3] = [0xC2, 0x20, 0x13];
pub const CS: Pin = (C, 12);
const BUSY_POLLS: u8 = 2;

pub struct Nor {
    pub mem: Vec<u8>,
    cs_edges: Option<u32>,
    cmd: Option<u8>,
    args: Vec<u8>,
    addr: usize,
    wel: bool,
    busy: u8,
    deep: bool,
    pp_count: usize,
    pp_addr: usize,
    /// What it did, as qspi.py's "flash: ..." lines without the prefix.
    pub log: Vec<String>,
    /// Commands it refused (qspi.py's ErrorLog lines).
    pub errors: Vec<String>,
    /// The bus pins' alternate functions were checked (once a reset).
    pins_checked: bool,
    /// qspi.py's WarningLog lines: unknown commands, bytes with CS high.
    pub warnings: Vec<String>,
}

impl Default for Nor {
    fn default() -> Self {
        Nor {
            mem: vec![0xFF; SIZE],
            cs_edges: None,
            cmd: None,
            args: Vec::new(),
            addr: 0,
            wel: false,
            busy: 0,
            deep: false,
            pp_count: 0,
            pp_addr: 0,
            log: Vec::new(),
            errors: Vec::new(),
            pins_checked: false,
            warnings: Vec::new(),
        }
    }
}

fn name(cmd: u8) -> &'static str {
    match cmd {
        0x9F => "RDID",
        0x05 => "RDSR",
        0x06 => "WREN",
        0x04 => "WRDI",
        0x03 => "READ",
        0x0B => "FAST_READ",
        0x02 => "PP",
        0x20 => "SE",
        0xD8 => "BE",
        0x60 | 0xC7 => "CE",
        0xB9 => "DP",
        0xAB => "RDP",
        _ => "?",
    }
}

impl Nor {
    /// A system reset: the command state goes, the array stays.
    pub fn reset(&mut self) {
        let mem = std::mem::take(&mut self.mem);
        let log = std::mem::take(&mut self.log);
        let errors = std::mem::take(&mut self.errors);
        let warnings = std::mem::take(&mut self.warnings);
        *self = Nor {
            mem,
            log,
            errors,
            warnings,
            ..Nor::default()
        };
    }

    /// The whole chip to 0xFF, unlogged (qspi.py's 0xF8).
    pub fn erase_all(&mut self) {
        self.mem.fill(0xFF);
    }

    /// A little-endian word at `addr`, wrapping (qspi.py's 0xF4 read).
    pub fn word(&self, addr: usize) -> u32 {
        (0..4).fold(0, |v, i| {
            v | (self.mem[(addr + i) % SIZE] as u32) << (8 * i)
        })
    }

    /// Stores a word as is, unlogged (qspi.py's 0xF4 write).
    pub fn set_word(&mut self, addr: usize, v: u32) {
        for i in 0..4 {
            self.mem[(addr + i) % SIZE] = (v >> (8 * i)) as u8;
        }
    }

    fn status(&self) -> u8 {
        (self.busy != 0) as u8 | (self.wel as u8) << 1
    }

    /// The chip select went high: complete the command in progress.
    fn finish(&mut self) {
        match self.cmd {
            Some(c @ (0x03 | 0x0B)) if self.args.len() >= 3 => {
                let n = self.args.len() - 3 - (c == 0x0B) as usize;
                let a = (self.args[0] as usize) << 16
                    | (self.args[1] as usize) << 8
                    | self.args[2] as usize;
                self.log.push(format!("{} 0x{a:06x} n={n}", name(c)));
            }
            Some(c @ (0x9F | 0x06 | 0x04 | 0xB9 | 0xAB)) => self.log.push(name(c).to_string()),
            _ => {}
        }
        if self.cmd == Some(0x02) && self.pp_count != 0 {
            self.log
                .push(format!("PP 0x{:06x} n={}", self.pp_addr, self.pp_count));
            self.wel = false;
            self.busy = BUSY_POLLS;
        }
        self.cmd = None;
        self.args.clear();
        self.pp_count = 0;
    }

    fn erase(&mut self, start: usize, n: usize, what: &str) {
        self.log.push(format!("{what} 0x{start:06x}"));
        self.mem[start..start + n].fill(0xFF);
        self.wel = false;
        self.busy = BUSY_POLLS;
    }

    fn need_wel(&mut self, what: &str) -> bool {
        if self.busy != 0 {
            self.errors
                .push(format!("{what} while busy (WIP), ignored"));
            false
        } else if !self.wel {
            self.errors.push(format!("{what} without WREN, ignored"));
            false
        } else {
            true
        }
    }

    /// One byte from the controller; returns the byte the flash drives.
    fn byte(&mut self, b: u8) -> u8 {
        let Some(c) = self.cmd else {
            self.cmd = Some(b);
            self.args.clear();
            if self.deep && b != 0xAB {
                return 0xFF;
            }
            match b {
                0x06 if self.busy == 0 => self.wel = true,
                0x04 if self.busy == 0 => self.wel = false,
                0x60 | 0xC7 => {
                    if self.need_wel("CE") {
                        self.erase(0, SIZE, "CE");
                    }
                }
                0xB9 => self.deep = true,
                0xAB => self.deep = false,
                0x06 | 0x04 | 0x9F | 0x05 | 0x03 | 0x0B | 0x02 | 0x20 | 0xD8 => {}
                _ => self.warnings.push(format!("unknown command 0x{b:02x}")),
            }
            return 0xFF;
        };
        self.args.push(b);
        let n = self.args.len();
        match c {
            0x9F => JEDEC_ID.get(n - 1).copied().unwrap_or(0xFF),
            0x05 => {
                let v = self.status();
                self.busy = self.busy.saturating_sub(1);
                v
            }
            // The electronic ID after 3 dummy bytes.
            0xAB => {
                if n >= 4 {
                    JEDEC_ID[2] - 1
                } else {
                    0xFF
                }
            }
            0x03 | 0x0B | 0x02 | 0x20 | 0xD8 if n <= 3 => {
                if n == 3 {
                    let a = (self.args[0] as usize) << 16
                        | (self.args[1] as usize) << 8
                        | self.args[2] as usize;
                    self.addr = a % SIZE;
                    match c {
                        0x20 if self.need_wel("SE") => self.erase(self.addr & !0xFFF, 0x1000, "SE"),
                        0xD8 if self.need_wel("BE") => {
                            self.erase(self.addr & !0xFFFF, 0x10000, "BE")
                        }
                        0x02 => {
                            if self.need_wel("PP") {
                                self.pp_addr = self.addr;
                            } else {
                                // Ignore the data bytes.
                                self.cmd = Some(0xFE);
                            }
                        }
                        _ => {}
                    }
                }
                0xFF
            }
            0x0B if n == 4 => 0xFF, // the dummy byte
            0x03 | 0x0B => {
                if self.busy != 0 {
                    self.errors.push("READ while busy (WIP)".to_string());
                }
                let v = self.mem[self.addr];
                self.addr = (self.addr + 1) % SIZE;
                v
            }
            0x02 => {
                self.mem[self.addr] &= b;
                self.addr = (self.addr & !0xFF) | ((self.addr + 1) & 0xFF);
                self.pp_count += 1;
                0xFF
            }
            _ => 0xFF,
        }
    }
}

/// SCK, MISO and MOSI: QSPI_SCK, QSPI_MI_IO1, QSPI_MO_IO0 on AF5 (the
/// datasheet's alternate-function table).
const BUS_PINS: [Pin; 3] = [(B, 3), (B, 4), (B, 5)];
const AF_SPIM: u8 = 5;

impl SsiDevice for Nor {
    fn shift(&mut self, b: u8, _line: SsiLine, gpio: &Gpio) -> u8 {
        if !self.pins_checked {
            self.pins_checked = true;
            for p in BUS_PINS {
                if gpio.alternate(p) != Some(AF_SPIM) {
                    self.errors.push(format!(
                        "qspi: {p:?} on {:?}, not QSPI (AF5)",
                        gpio.alternate(p)
                    ));
                }
            }
        }
        let cs = gpio
            .watched(CS)
            .expect("the board watches the flash's chip select");
        if self.cs_edges != Some(cs.rising) {
            self.cs_edges = Some(cs.rising);
            self.finish();
        }
        if cs.level {
            self.warnings
                .push(format!("qspi: byte 0x{b:02x} sent with flash CS high"));
            return 0xFF;
        }
        self.byte(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_pins_must_be_on_af5() {
        let line = SsiLine { cr0: 7, baudr: 32 };
        let mut g = Gpio::default();
        g.watch(CS);
        // B3-B5 to AF5 (MODER 2, AFRL nibbles 3-5); then B4 back to AF0.
        g.write(0x400, 0b10_1010 << 6, &mut |_| {});
        g.write(0x420, 0x555 << 12, &mut |_| {});
        let mut n = Nor::default();
        n.shift(0x9F, line, &g);
        assert_eq!(n.errors, Vec::<String>::new());
        g.write(0x420, 0x505 << 12, &mut |_| {});
        let mut n = Nor::default();
        n.shift(0x9F, line, &g);
        assert_eq!(n.errors, ["qspi: (1, 4) on Some(0), not QSPI (AF5)"]);
    }
}
