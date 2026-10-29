//! The Akko 5075B Plus on wb32-sim, and the harness the tests drive it with.

use cortex_m3_emu::{Bus, Config, Cpu, Size, TICK_HZ};
use std::path::PathBuf;
use wb32_sim::chip::ROM_BASE;
use wb32_sim::gpio::{A, B, C, Matrix, Pin};
use wb32_sim::{LoadError, Wb32};

/// Where the ROM stub's reset handler spins: "PC is here" means "entered DFU".
pub const ROM_PARKED: u32 = 0x1FFF_E008;

/// The key matrix (ROW2COL), as ../../sim/peripherals/gpio.py: rows
/// A0-A4, C13; columns C0-C3, A6, B10-B14, A10, C6-C9.
pub const ROWS: [Pin; 6] = [(A, 0), (A, 1), (A, 2), (A, 3), (A, 4), (C, 13)];
pub const COLS: [Pin; 15] = [
    (C, 0),
    (C, 1),
    (C, 2),
    (C, 3),
    (A, 6),
    (B, 10),
    (B, 11),
    (B, 12),
    (B, 13),
    (B, 14),
    (A, 10),
    (C, 6),
    (C, 7),
    (C, 8),
    (C, 9),
];

/// The firmware images sim-rs/run.sh snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Image {
    /// boot-guard with `always-dfu`: bounces straight into the ROM.
    M1aBounce,
    M1bStub,
    M2Blink,
    M3Usb,
    AkkoRmk,
    /// akko-rmk built with the `mac` feature.
    AkkoRmkMac,
}

impl Image {
    pub const ALL: [Image; 6] = [
        Image::M1aBounce,
        Image::M1bStub,
        Image::M2Blink,
        Image::M3Usb,
        Image::AkkoRmk,
        Image::AkkoRmkMac,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            Image::M1aBounce => "m1a-bounce",
            Image::M1bStub => "m1b-stub",
            Image::M2Blink => "m2-blink",
            Image::M3Usb => "m3-usb",
            Image::AkkoRmk => "akko-rmk",
            Image::AkkoRmkMac => "akko-rmk-mac",
        }
    }

    /// In `$AKKO_SIM_IMAGES`, by default sim-rs/target/images.
    pub fn path(self) -> PathBuf {
        let dir = std::env::var_os("AKKO_SIM_IMAGES")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/images")
            });
        dir.join(self.file_name())
    }

    /// The address of the first symbol whose (mangled) name contains
    /// `part`, e.g. "11time_driver6DRIVER".
    pub fn symbol(self, part: &str) -> Option<u32> {
        use object::{Object, ObjectSymbol};
        let data = self.read().ok()?;
        let file = object::File::parse(&*data).ok()?;
        file.symbols()
            .find(|s| s.name().is_ok_and(|n| n.contains(part)))
            .map(|s| s.address() as u32)
    }

    pub fn read(self) -> Result<Vec<u8>, LoadError> {
        let path = self.path();
        std::fs::read(&path).map_err(|e| {
            LoadError(format!(
                "{}: {e} (sim-rs/run.sh builds the images)",
                path.display()
            ))
        })
    }
}

pub struct Board {
    pub cpu: Cpu,
    pub chip: Wb32,
    /// System resets (SYSRESETREQ) since power-on.
    pub resets: u32,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    /// A blank board with the ROM stub in place of the boot ROM (we don't
    /// ship its contents): initial SP, then a reset handler that spins at
    /// ROM_PARKED. As ../sim/akko.resc.
    pub fn new() -> Self {
        let mut chip = Wb32::with_matrix(Matrix {
            rows: ROWS.to_vec(),
            cols: COLS.to_vec(),
        });
        let mut stub = Vec::new();
        stub.extend_from_slice(&0x2000_1000u32.to_le_bytes());
        stub.extend_from_slice(&(ROM_PARKED | 1).to_le_bytes());
        stub.extend_from_slice(&0xE7FEu16.to_le_bytes()); // b .
        chip.load(ROM_BASE, &stub).expect("the stub fits the ROM");
        Board {
            cpu: Cpu::new(Config::default()),
            chip,
            resets: 0,
        }
    }

    /// Loads an image and resets into it, as `sysbus LoadELF` then
    /// `machine Reset`.
    pub fn boot(image: Image) -> Result<Board, LoadError> {
        let mut b = Board::new();
        b.load(image)?;
        b.reset();
        Ok(b)
    }

    /// A system reset: SRAM keeps its contents, as on the chip.
    pub fn reset(&mut self) {
        self.chip.reset();
        self.cpu.reset(&mut self.chip);
    }

    /// Runs for `secs` of emulated time. A SYSRESETREQ resets the system
    /// and carries on.
    pub fn run_for(&mut self, secs: f64) {
        let end = self.cpu.now + (secs * TICK_HZ as f64) as u64;
        while self.cpu.now < end && !self.cpu.halted {
            self.cpu.run_until(&mut self.chip, end);
            if std::mem::take(&mut self.cpu.reset_requested) {
                self.resets += 1;
                self.reset();
            }
        }
    }

    pub fn pc(&self) -> u32 {
        self.cpu.pc()
    }

    /// Parked in the ROM stub: "entered DFU".
    pub fn in_rom(&self) -> bool {
        self.pc() == ROM_PARKED
    }

    pub fn in_flash(&self) -> bool {
        (0x0800_0000..0x0804_0000).contains(&self.pc())
    }

    /// A word as the debugger reads it: memory, peripherals or the core's
    /// own registers.
    pub fn word(&mut self, addr: u32) -> u32 {
        if addr >= 0xE000_0000 {
            let now = self.cpu.now;
            return self.cpu.ppb.read(addr, Size::Word, now, &mut self.cpu.diags);
        }
        self.chip.read(addr, Size::Word)
    }

    /// Presses or releases the key at (row, col).
    pub fn key(&mut self, row: u8, col: u8, down: bool) {
        if down {
            self.chip.gpio.pressed.insert((row, col));
        } else {
            self.chip.gpio.pressed.remove(&(row, col));
        }
        self.chip.gpio.update();
    }

    /// Holds a pin at a level from outside, or lets it go (`None`).
    pub fn drive(&mut self, pin: Pin, level: Option<bool>) {
        match level {
            Some(l) => self.chip.gpio.driven.insert(pin, l),
            None => self.chip.gpio.driven.remove(&pin),
        };
        self.chip.gpio.update();
    }

    /// Loads an image's ELF. Returns its entry point.
    pub fn load(&mut self, image: Image) -> Result<u32, LoadError> {
        let data = image.read()?;
        self.chip.load_elf(&data)
    }
}
