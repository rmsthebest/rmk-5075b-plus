//! The Akko 5075B Plus on wb32-sim, and the harness the tests drive it with.

use cortex_m3_emu::{Config, Cpu, TICK_HZ};
use std::path::PathBuf;
use wb32_sim::chip::ROM_BASE;
use wb32_sim::{LoadError, Wb32};

/// Where the ROM stub's reset handler spins: "PC is here" means "entered DFU".
pub const ROM_PARKED: u32 = 0x1FFF_E008;

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
        let mut chip = Wb32::new();
        let mut stub = Vec::new();
        stub.extend_from_slice(&0x2000_1000u32.to_le_bytes());
        stub.extend_from_slice(&(ROM_PARKED | 1).to_le_bytes());
        stub.extend_from_slice(&0xE7FEu16.to_le_bytes()); // b .
        chip.load(ROM_BASE, &stub).expect("the stub fits the ROM");
        Board {
            cpu: Cpu::new(Config::default()),
            chip,
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
        self.cpu.reset(&mut self.chip);
    }

    /// Runs for `secs` of emulated time. A SYSRESETREQ resets the system
    /// and carries on.
    pub fn run_for(&mut self, secs: f64) {
        let end = self.cpu.now + (secs * TICK_HZ as f64) as u64;
        while self.cpu.now < end && !self.cpu.halted {
            self.cpu.run_until(&mut self.chip, end);
            if std::mem::take(&mut self.cpu.reset_requested) {
                self.reset();
            }
        }
    }

    /// Loads an image's ELF. Returns its entry point.
    pub fn load(&mut self, image: Image) -> Result<u32, LoadError> {
        let data = image.read()?;
        self.chip.load_elf(&data)
    }
}
