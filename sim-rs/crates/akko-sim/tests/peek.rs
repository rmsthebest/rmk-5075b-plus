//! ../../rom/sim/tests/peek.robot: the rom-peek feature (firmware/src/main.rs
//! `peek`, rom/tools/rompeek.py) on the board: akko-rmk built with
//! `--features rom-peek`.

use akko_sim::usb_host::{UsbEvent, cmd, hex};
use akko_sim::{Board, Image};
use cortex_m3_emu::{Bus, Size};

struct T {
    b: Board,
    cursor: usize,
}

impl T {
    fn boot_and_enumerate() -> T {
        let mut t = T {
            b: Board::boot(Image::AkkoRmkRomPeek).unwrap(),
            cursor: 0,
        };
        t.hid_or_log(
            4.0,
            "configured",
            |e| matches!(e, UsbEvent::Log(l) if l.starts_with("configured")),
        );
        t.b.run_for(0.05);
        t
    }

    fn hid_or_log(&mut self, secs: f64, what: &str, pred: impl FnMut(&UsbEvent) -> bool) {
        if self.b.wait_for(&mut self.cursor, secs, pred).is_none() {
            let tail: Vec<String> = self
                .b
                .host
                .events()
                .iter()
                .rev()
                .take(8)
                .map(|e| e.to_string())
                .collect();
            panic!("no {what} within {secs} s; last events, newest first: {tail:#?}");
        }
    }

    fn raw_hid_send(&mut self, bytes: &[u8]) {
        let mut p = bytes.to_vec();
        p.resize(32, 0);
        for b in p {
            self.b.host.out_byte(b);
        }
        self.b.host.command(cmd::OUT_PACKET | 2 << 8);
    }

    fn hid_should_show(&mut self, want: &str) {
        self.hid_or_log(
            1.0,
            want,
            |e| matches!(e, UsbEvent::Hid { ep: 3, data } if hex(data).starts_with(want)),
        );
    }

    /// Vial channel 0xDA at `addr`: the answer echoes the address, then
    /// `status` and `count`, then `count` bytes of memory, then zeros.
    fn peek_should_show(&mut self, addr: u32, status: u8, count: u8) {
        let a = addr.to_le_bytes();
        self.raw_hid_send(&[0x08, 0xDA, a[0], a[1], a[2], a[3]]);
        let mut want = vec![0x08, 0xDA];
        want.extend(a);
        want.extend([status, count]);
        for w in (0..count as u32).step_by(4) {
            want.extend(self.b.chip.read(addr + w, Size::Word).to_le_bytes());
        }
        want.resize(32, 0);
        self.hid_should_show(&hex(&want));
    }
}

/// Vial channel 0xDA reads the ROM bootloader (0x1FFFE000-0x1FFFEFFF) and
/// the information blocks (0x1FFFF000-0x1FFFF7FF), 24 bytes at a time and
/// no further than the end of a range. An unaligned address or one outside
/// both is refused and nothing is read. The sim maps 0x1FFFE000-0x1FFFFFFF
/// as memory, so a pattern written there shows whether it was read.
#[test]
fn the_rom_peek_reads_only_the_rom_and_the_information_blocks() {
    let mut t = T::boot_and_enumerate();
    for addr in [
        0x1FFF_E100u32,
        0x1FFF_E104,
        0x1FFF_E108,
        0x1FFF_E10C,
        0x1FFF_E110,
        0x1FFF_E114,
        0x1FFF_EFF8,
        0x1FFF_EFFC,
        0x1FFF_F000,
        0x1FFF_F7F0,
        0x1FFF_F7F4,
        0x1FFF_F7F8,
        0x1FFF_F7FC,
        0x1FFF_F800,
    ] {
        t.b.chip
            .rom_mut()
            .write(addr - 0x1FFF_E000, Size::Word, addr ^ 0xA5C3_E10F);
    }
    t.peek_should_show(0x1FFF_E100, 0, 24);
    // The last 8 bytes of the ROM; the information blocks are the next read.
    t.peek_should_show(0x1FFF_EFF8, 0, 8);
    t.peek_should_show(0x1FFF_F7F0, 0, 16);
    // Past the information blocks, unaligned, and SRAM: refused, nothing read.
    t.peek_should_show(0x1FFF_F800, 1, 0);
    t.peek_should_show(0x1FFF_E101, 1, 0);
    t.peek_should_show(0x2000_0000, 1, 0);
    t.raw_hid_send(&[0x08, 0xD2, 0x00]);
    t.hid_should_show("08 d2 00 00");
    assert_eq!(t.b.errors(), Vec::<String>::new());
}
