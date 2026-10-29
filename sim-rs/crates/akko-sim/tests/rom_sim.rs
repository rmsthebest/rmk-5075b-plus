//! ../../rom/sim/tests: the ROM bootloader's vendor bulk pipe, with the
//! command set in rom/notes.md ("DFU state machine"), on both the chip's ROM
//! (the dump in $ROM_WORK) and rom/rust's build. All multi-byte fields are
//! little-endian. peek.robot (the firmware's rom-peek channel) is in peek.rs.

use akko_sim::usb_host::{UsbEvent, cmd, hex};
use akko_sim::{Board, Rom};

const BULK_EP: u32 = 1;
const APP: u32 = 0x0800_0000;
/// A 16-byte app: SP 0x20001000, reset 0x08000009, then `b .` at 0x08000008.
const TINY_APP: [u8; 16] = [
    0x00, 0x10, 0x00, 0x20, 0x09, 0x00, 0x00, 0x08, 0xFE, 0xE7, 0, 0, 0, 0, 0, 0,
];

struct T {
    b: Board,
    cursor: usize,
}

impl T {
    fn start(rom: Rom) -> T {
        T {
            b: Board::start_rom(rom).unwrap_or_else(|e| panic!("{e}")),
            cursor: 0,
        }
    }

    fn wait(&mut self, secs: f64, what: &str, pred: impl FnMut(&UsbEvent) -> bool) {
        if self.b.wait_for(&mut self.cursor, secs, pred).is_none() {
            let tail: Vec<String> = self
                .b
                .host
                .events()
                .iter()
                .rev()
                .take(12)
                .map(|e| e.to_string())
                .collect();
            panic!("no {what} within {secs} s; last events, newest first: {tail:#?}");
        }
    }

    fn log_should_show(&mut self, line: &str, secs: f64) {
        self.wait(secs, line, |e| matches!(e, UsbEvent::Log(l) if l == line));
    }

    fn wait_for_enumeration(&mut self) {
        self.log_should_show("configured: if 0 class ff eps [81 01]", 3.0);
    }

    /// One command on bulk OUT EP1, split into 64-byte packets.
    fn bulk_send(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.b.host.out_byte(b);
        }
        self.b.host.command(cmd::BULK_OUT | BULK_EP << 8);
    }

    /// `<cmd> <sub> 00 00 <a LE> <b LE> [data]`: the updater's layout.
    fn command(&mut self, c: u8, a: u32, b: u32, data: &[u8]) {
        let mut all = vec![c, 0, 0, 0];
        all.extend(a.to_le_bytes());
        all.extend(b.to_le_bytes());
        all.extend(data);
        self.bulk_send(&all);
    }

    /// The next reply packet on bulk IN EP1.
    fn reply_should_be(&mut self, want: &str) {
        self.wait(
            1.0,
            &format!("reply {want}"),
            |e| matches!(e, UsbEvent::Bulk { ep: 1, data } if hex(data) == want),
        );
    }

    fn no_host_errors(&self) {
        assert_eq!(self.b.host_errors(), Vec::<String>::new());
    }

    fn download_the_tiny_app(&mut self) {
        self.command(0x5E, APP, 1, &[]);
        self.reply_should_be("5e 00");
        self.command(0x6A, APP, 16, &TINY_APP);
        self.reply_should_be("6a 00");
    }
}

// enumerate.robot

/// 342d:dfa0, "Westberry Tech." / "WB Device in DFU Mode", one vendor
/// interface with bulk EP1 IN/OUT.
fn the_rom_enumerates_as_the_vendor_bootloader(rom: Rom) {
    let mut t = T::start(rom);
    t.log_should_show("device 342d:dfa0, EP0 64 bytes", 3.0);
    t.log_should_show("string 2 = 'WB Device in DFU Mode'", 1.0);
    t.log_should_show("string 1 = 'Westberry Tech.'", 1.0);
    t.wait_for_enumeration();
    t.no_host_errors();
}

/// 01 00 -> 01 00 0d <version 04> <SYS_ID> <flash bytes> <sram bytes>.
fn get_info_reports_the_chip(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.bulk_send(&[0x01, 0x00]);
    t.reply_should_be("01 00 0d 04 80 e9 50 3a 00 00 04 00 00 90 00 00");
}

fn an_unknown_command_is_invalid(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.bulk_send(&[0x55, 0x00]);
    t.reply_should_be("55 ff");
}

// download.robot

fn a_page_is_erased_written_and_read_back(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.command(0x5E, APP, 1, &[]);
    t.reply_should_be("5e 00");
    assert!(
        t.b.chip.fmc.ops.contains(&(0x08, APP)),
        "{:?}",
        t.b.chip.fmc.ops
    );
    t.command(0x6A, APP, 16, &TINY_APP);
    t.reply_should_be("6a 00");
    assert!(
        t.b.chip.fmc.ops.ends_with(&[(0x0C, APP)]),
        "{:?}",
        t.b.chip.fmc.ops
    );
    assert_eq!(t.b.word(0x0800_0004), 0x0800_0009);
    t.command(0x72, APP, 16, &[]);
    t.reply_should_be(&format!("72 00 {}", hex(&TINY_APP)));
}

/// The ROM itself and the factory blocks 0-3 aren't reachable (address
/// check 0x1FFFE8CC): ARGERR.
fn reads_outside_flash_and_blocks_4_7_are_refused(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.command(0x72, 0x1FFF_E000, 16, &[]);
    t.reply_should_be("72 01");
    t.command(0x72, 0x1FFF_F000, 16, &[]);
    t.reply_should_be("72 01");
    t.command(0x72, 0x1FFF_F400, 4, &[]);
    t.reply_should_be("72 00 00 00 00 00");
}

fn an_erase_outside_flash_is_refused(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.command(0x5E, 0x1FFF_F400, 1, &[]);
    t.reply_should_be("5e 01");
    assert!(t.b.chip.fmc.ops.is_empty(), "{:?}", t.b.chip.fmc.ops);
}

// leave.robot

/// 9b .. <addr> <ms>: acks, takes USB down (the host sees a detach), sets
/// VTOR = 0 and MSP = [addr], jumps to [addr + 4].
fn go_jumps_to_the_app_with_vtor_at_0(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.download_the_tiny_app();
    t.command(0x9B, APP, 0, &[]);
    t.reply_should_be("9b 00");
    t.log_should_show("device detached", 1.0);
    t.b.run_for(0.05);
    assert_eq!(t.b.pc(), 0x0800_0008);
    assert_eq!(t.b.cpu.r[13], 0x2000_1000);
    assert_eq!(t.b.word(0xE000_ED08), 0);
}

fn go_outside_flash_is_refused(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.command(0x9B, 0x2000_0000, 0, &[]);
    t.reply_should_be("9b 01");
}

/// 83 .. <ms>: acks, takes USB down, waits, then SYSRESETREQ. Booting from
/// flash (BOOT0 = 0) then runs the downloaded app.
fn reset_resets_the_chip(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    t.download_the_tiny_app();
    t.command(0x83, 10, 0, &[]);
    t.reply_should_be("83 00");
    t.log_should_show("device detached", 1.0);
    t.b.run_for(0.1);
    assert_eq!(t.b.pc(), 0x0800_0008);
}

/// Its clocks once enumerated (the clock tree from the RM, sysctl.rs):
/// 48 MHz for the core and APB1, APB2 left off. rom/rust's must match.
fn the_rom_runs_at_48_mhz_with_apb2_off(rom: Rom) {
    let mut t = T::start(rom);
    t.wait_for_enumeration();
    let c = t.b.chip.sysctl.clocks();
    assert_eq!(
        (c.mainclk, c.hclk, c.pclk1, c.pclk2),
        (48_000_000, 48_000_000, 48_000_000, 0)
    );
    assert_eq!(t.b.chip.take_diags(), vec![]);
}

macro_rules! on_both {
    ($($name:ident),* $(,)?) => {
        mod dump {
            $(#[test] fn $name() { super::$name(akko_sim::Rom::Dump) })*
        }
        mod rust {
            $(#[test] fn $name() { super::$name(akko_sim::Rom::Rust) })*
        }
    };
}

on_both!(
    the_rom_enumerates_as_the_vendor_bootloader,
    get_info_reports_the_chip,
    an_unknown_command_is_invalid,
    a_page_is_erased_written_and_read_back,
    reads_outside_flash_and_blocks_4_7_are_refused,
    an_erase_outside_flash_is_refused,
    go_jumps_to_the_app_with_vtor_at_0,
    go_outside_flash_is_refused,
    reset_resets_the_chip,
    the_rom_runs_at_48_mhz_with_apb2_off,
);
