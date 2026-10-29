//! ../../rom/sim/traces: the ROM's peripheral write traces from Renode
//! (../../rom/sim/trace.py), one line per write in order, "<peripheral>+0x<off>
//! = 0x<value>", which our run of the same scenario must match line for
//! line, on the ROM dump and on rom/rust's build alike (with Renode's
//! folding of repeated lines, and where Renode is wrong, below).
//!   boot      reset into the image, run 1 s (the host enumerates it)
//!   download  boot, then ERASE 0x08000000, WRITE_PAGE (a 16-byte app), READ
//!   leave     download, then GO 0x08000000

use akko_sim::usb_host::cmd;
use akko_sim::{Board, Rom};

const APP: [u8; 16] = [0x00, 0x10, 0x00, 0x20, 0x09, 0x00, 0x00, 0x08, 0xFE, 0xE7, 0, 0, 0, 0, 0, 0];

/// One command on bulk OUT EP1, then 50 ms, as trace.py's `command`.
fn command(b: &mut Board, c: u8, a: u32, len: u32, data: &[u8]) {
    let mut all = vec![c, 0, 0, 0];
    all.extend(a.to_le_bytes());
    all.extend(len.to_le_bytes());
    all.extend(data);
    for x in all {
        b.host.out_byte(x);
    }
    b.host.command(cmd::BULK_OUT | 1 << 8);
    b.run_for(0.05);
}

/// The lines, each with the PC that wrote it (0 for comments).
fn trace(rom: Rom, scenario: &str) -> Vec<(String, u32)> {
    let mut b = Board::start_rom(rom).unwrap_or_else(|e| panic!("{e}"));
    b.chip.bus_writes = Some(Vec::new());
    b.run_for(1.0);
    if scenario != "boot" {
        command(&mut b, 0x5E, 0x0800_0000, 1, &[]);
        command(&mut b, 0x6A, 0x0800_0000, APP.len() as u32, &APP);
        command(&mut b, 0x72, 0x0800_0000, APP.len() as u32, &[]);
    }
    if scenario == "leave" {
        command(&mut b, 0x9B, 0x0800_0000, 0, &[]);
        b.run_for(0.1);
    }
    let mut lines: Vec<(String, u32)> =
        b.chip.bus_writes.take().unwrap().iter().map(|w| (w.to_string(), w.pc)).collect();
    // trace.py keeps these as comments.
    lines.extend(b.host_errors().into_iter().map(|e| (format!("# {e}"), 0)));
    lines.extend(b.chip.fmc.errors.iter().filter(|e| e.starts_with("FMC ERROR")).map(|e| (format!("# {e}"), 0)));
    lines
}

/// Where Renode is wrong and our line is the chip's: (Renode's, ours).
const RENODE_DIFFERS: [(&str, &str); 1] = [
    // The ROM's priority grouping is a read-modify-write of AIRCR (rom/rust
    // clock.rs: `(aircr & 0xF8FF) | 0x300 | 0x05FA_0000`). Renode's AIRCR
    // reads with bit 13 set, which ARMv7-M reserves (RAZ).
    ("nvic+0xd0c = 0x5fa2300", "nvic+0xd0c = 0x5fa0300"),
];

/// Renode's log writes a run of identical lines (the PC they carry
/// included) as the line, then the line again with " (N)" for the N
/// repeats; trace.py keeps both and drops the count. So a run of two or
/// more is two lines in the reference.
fn renode_folded(lines: Vec<(String, u32)>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<(String, u32)> = None;
    let mut run = 0;
    for l in lines {
        if last.as_ref() == Some(&l) {
            run += 1;
            if run == 1 {
                out.push(l.0.clone());
            }
        } else {
            run = 0;
            out.push(l.0.clone());
            last = Some(l);
        }
    }
    out
}

fn check(rom: Rom, scenario: &str) {
    let path = format!("{}/../../../rom/sim/traces/{scenario}.txt", env!("CARGO_MANIFEST_DIR"));
    let want: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| RENODE_DIFFERS.iter().find(|d| d.0 == l).map_or(l, |d| d.1).to_string())
        .collect();
    let ours = renode_folded(trace(rom, scenario));
    if let Some(i) = (0..want.len().max(ours.len())).find(|&i| want.get(i) != ours.get(i)) {
        let show = |v: &[String]| v[i.saturating_sub(3)..(i + 4).min(v.len())].join("\n  ");
        panic!(
            "{rom:?} {scenario}: line {} differs ({} lines Renode, {} ours)\nRenode:\n  {}\nours:\n  {}",
            i + 1,
            want.len(),
            ours.len(),
            show(&want),
            show(&ours)
        );
    }
}

#[test]
fn boot_dump() {
    check(Rom::Dump, "boot");
}
#[test]
fn download_dump() {
    check(Rom::Dump, "download");
}
#[test]
fn leave_dump() {
    check(Rom::Dump, "leave");
}
#[test]
fn boot_rust() {
    check(Rom::Rust, "boot");
}
#[test]
fn download_rust() {
    check(Rom::Rust, "download");
}
#[test]
fn leave_rust() {
    check(Rom::Rust, "leave");
}
