//! The Akko 5075B Plus on wb32-sim, and the harness the tests drive it with.

pub mod usb_host;

use cortex_m3_emu::{Bus, Config, Cpu, IrqLines, Size, TICK_HZ};
use std::path::PathBuf;
use wb32_sim::chip::ROM_BASE;
use wb32_sim::gpio::{A, B, C, Matrix, Pin};
use usb_host::{Host, UsbEvent};
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

/// A ROM bootloader to run in place of the boot ROM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rom {
    /// The chip's own, from `$ROM_WORK/dump` (by default ../wb32-rom-re
    /// beside the repo; ../../rom/README.md). Never copied into the repo.
    Dump,
    /// rom/rust's build, linked at 0x1FFF_E000 (snapshot by run.sh),
    /// loaded over the dump.
    Rust,
}

impl Rom {
    pub fn dump_dir() -> PathBuf {
        std::env::var_os("ROM_WORK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../wb32-rom-re"))
            .join("dump")
    }

    pub fn rust_path() -> PathBuf {
        Image::M1bStub.path().with_file_name("wb32-rom-dfu")
    }
}

pub struct Board {
    pub cpu: Cpu,
    pub chip: Wb32,
    /// The USB host on the board's port (started again by a system
    /// reset, as Renode runs usb.py's init again).
    pub host: Host,
    /// System resets (SYSRESETREQ) since power-on.
    pub resets: u32,
}

/// The chip, with the USB host stepped alongside it.
struct BoardBus<'a> {
    chip: &'a mut Wb32,
    host: &'a mut Host,
}

impl Bus for BoardBus<'_> {
    fn read(&mut self, addr: u32, size: Size) -> u32 {
        self.chip.read(addr, size)
    }
    fn write(&mut self, addr: u32, size: Size, value: u32) {
        self.chip.write(addr, size, value)
    }
    fn fetch(&mut self, addr: u32) -> Option<u16> {
        self.chip.fetch(addr)
    }
    fn advance(&mut self, now: u64) {
        self.chip.advance(now);
        while self.host.next_at <= now {
            self.host.step(self.chip);
        }
    }
    fn next_event(&self) -> u64 {
        self.chip.next_event().min(self.host.next_at)
    }
    fn irq_lines(&mut self) -> IrqLines {
        self.chip.irq_lines()
    }
    fn executing(&mut self, pc: u32) {
        self.chip.executing(pc)
    }
    fn ppb_write(&mut self, addr: u32, size: Size, value: u32) {
        self.chip.ppb_write(addr, size, value)
    }
    fn sleep(&mut self, deep: bool) {
        self.chip.sleep(deep)
    }
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
        let host = Host::new(chip.usb.clone(), 0);
        Board {
            cpu: Cpu::new(Config::default()),
            chip,
            host,
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
        let undivided = self.host.undivided_usbpre_ok;
        self.host = Host::new(self.chip.usb.clone(), self.cpu.now);
        self.host.undivided_usbpre_ok = undivided;
        self.cpu.reset(&mut self.chip);
    }

    /// A board running a ROM bootloader, entered as ../../rom/sim/rom.resc
    /// does: a reset, then SP and PC from the ROM's vector table (the
    /// dump's are 0x2000_0450 and 0x1FFF_E091), as boot-guard's jump. The
    /// host takes the ROM's 48 MHz USB clock (USBPRE 0x08).
    pub fn start_rom(rom: Rom) -> Result<Board, LoadError> {
        let mut b = Board::new();
        // The dump always, as rom.resc: rom/rust reads the information
        // blocks too. Its build then goes over the ROM.
        let dir = Rom::dump_dir();
        for (file, addr) in [("rom.bin", ROM_BASE), ("info.bin", 0x1FFF_F000)] {
            let bytes = std::fs::read(dir.join(file))
                .map_err(|e| LoadError(format!("{}: {e} (../rom/README.md)", dir.join(file).display())))?;
            b.chip.load(addr, &bytes)?;
        }
        if rom == Rom::Rust {
            let path = Rom::rust_path();
            let data = std::fs::read(&path)
                .map_err(|e| LoadError(format!("{}: {e} (sim-rs/run.sh builds it)", path.display())))?;
            b.chip.load_elf(&data)?;
        }
        b.host.undivided_usbpre_ok = true;
        b.reset();
        b.cpu.set_msp(b.chip.read(ROM_BASE, Size::Word));
        let pc = b.chip.read(ROM_BASE + 4, Size::Word);
        b.cpu.r[15] = pc & !1;
        b.cpu.thumb = pc & 1 != 0;
        Ok(b)
    }

    /// What a debugger's SYSRESETREQ does (wb32-dfu-updater_cli -R):
    /// a system reset, RAM kept.
    pub fn system_reset(&mut self) {
        self.resets += 1;
        self.reset();
    }

    /// Runs for `secs` of emulated time. A SYSRESETREQ resets the system
    /// and carries on.
    pub fn run_for(&mut self, secs: f64) {
        let end = self.cpu.now + (secs * TICK_HZ as f64) as u64;
        while self.cpu.now < end && !self.cpu.halted {
            let mut bus = BoardBus {
                chip: &mut self.chip,
                host: &mut self.host,
            };
            self.cpu.run_until(&mut bus, end);
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

    /// Runs until a USB event after `cursor` matches, for at most `secs`,
    /// in host steps. On a match, `cursor` moves past it (later waits only
    /// see later events, as Renode's log tester).
    pub fn wait_for(
        &mut self,
        cursor: &mut usize,
        secs: f64,
        mut pred: impl FnMut(&UsbEvent) -> bool,
    ) -> Option<UsbEvent> {
        let end = self.cpu.now + (secs * TICK_HZ as f64) as u64;
        loop {
            {
                let events = self.host.events();
                if let Some(i) = events[*cursor..].iter().position(&mut pred) {
                    *cursor += i + 1;
                    return Some(events[*cursor - 1].clone());
                }
            }
            if self.cpu.now >= end || self.cpu.halted {
                return None;
            }
            self.run_for(usb_host::STEP as f64 / TICK_HZ as f64);
        }
    }

    /// Host errors so far, as text.
    pub fn host_errors(&self) -> Vec<String> {
        self.host
            .events()
            .iter()
            .filter(|e| matches!(e, UsbEvent::Error(_)))
            .map(|e| e.to_string())
            .collect()
    }

    /// Loads an image's ELF. Returns its entry point.
    pub fn load(&mut self, image: Image) -> Result<u32, LoadError> {
        let data = image.read()?;
        self.chip.load_elf(&data)
    }
}
