//! The decoder against LLVM's disassembler (llvm-objdump from the pinned
//! toolchain's llvm-tools, the same LLVM that assembled the images). Both
//! disassemble the same bytes and must agree on every instruction, once
//! immediates are numbers and `.w`/`.n` are dropped:
//! - every image's code;
//! - the ROM dump swept linearly, data and all (skipped with a note if the
//!   dump isn't on this machine: it stays outside the repo);
//! - a seeded random blob, for encodings no compiler here emits.
//!
//! Where we say <unknown> LLVM may name a coprocessor op, which this core
//! doesn't have; those are counted, not failures. So are encodings the
//! decoder flags UNPREDICTABLE (a should-be bit broken), which LLVM rejects
//! or accepts inconsistently: either reading passes for those only.

use akko_sim::Image;
use cortex_m3_emu::decode::{ItState, Op, decode};
use cortex_m3_emu::disasm;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn llvm_tool(name: &str) -> PathBuf {
    let out = |args: &[&str]| {
        let o = Command::new("rustc").args(args).output().expect("rustc");
        String::from_utf8(o.stdout).unwrap()
    };
    let sysroot = out(&["--print", "sysroot"]);
    let host = out(&["-vV"])
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(String::from))
        .unwrap();
    let p = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(name);
    assert!(
        p.exists(),
        "{} missing: rustup component add llvm-tools",
        p.display()
    );
    p
}

fn scratch() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("decode-oracle");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Wraps a raw blob in an ELF at `base`, as code, for llvm-objdump.
fn blob_elf(name: &str, data: &[u8], base: u32) -> PathBuf {
    let dir = scratch();
    let bin = dir.join(format!("{name}.bin"));
    let elf = dir.join(format!("{name}.elf"));
    std::fs::write(&bin, data).unwrap();
    let st = Command::new(llvm_tool("llvm-objcopy"))
        .args(["-I", "binary", "-O", "elf32-littlearm"])
        .arg(format!("--change-section-address=.data=0x{base:x}"))
        .args(["--set-section-flags", ".data=code,alloc"])
        .arg(&bin)
        .arg(&elf)
        .status()
        .unwrap();
    assert!(st.success());
    elf
}

/// Every number as decimal, `.w`/`.n` and comments gone, `#0]` folded.
fn normalize(text: &str) -> String {
    let text = text.split(['@', '<']).next().unwrap_or("").trim();
    let text = if text.is_empty() { "<unknown>" } else { text };
    let (mnem, ops) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let mnem = mnem.trim_end_matches(".w").trim_end_matches(".n");
    let mut out = String::new();
    let b = ops.trim().as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let prev_alnum = i > 0 && (b[i - 1] as char).is_ascii_alphanumeric();
        let neg = c == '-' && i + 1 < b.len() && b[i + 1].is_ascii_digit();
        if (c.is_ascii_digit() || neg) && !prev_alnum {
            let start = if neg { i + 1 } else { i };
            let (radix, mut j) = if b[start..].starts_with(b"0x") {
                (16, start + 2)
            } else {
                (10, start)
            };
            let digits = j;
            while j < b.len() && (b[j] as char).is_digit(radix) {
                j += 1;
            }
            let v =
                i64::from_str_radix(std::str::from_utf8(&b[digits..j]).unwrap(), radix).unwrap();
            out.push_str(&(if neg { -v } else { v }).to_string());
            i = j;
        } else if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    let out = out.replace(", #0]", "]");
    format!("{mnem} {out}").trim().to_string()
}

const COPROCESSOR: [&str; 14] = [
    "mcr", "mcr2", "mrc", "mrc2", "mcrr", "mcrr2", "mrrc", "mrrc2", "cdp", "cdp2", "ldc", "ldc2",
    "stc", "stc2",
];

/// Whether an IT block has an else: for AL that's UNPREDICTABLE.
fn it_has_else(firstcond: u8, mask: u8) -> bool {
    cortex_m3_emu::disasm::it_suffix(firstcond, mask).contains('e')
}

#[derive(Default)]
struct Report {
    compared: usize,
    coprocessor: usize,
    unpredictable: usize,
    flagged: usize,
    mismatches: BTreeMap<String, Vec<String>>,
}

impl Report {
    fn check(&mut self, what: &str, path: &Path, triple_flags: &[&str]) {
        let out = Command::new(llvm_tool("llvm-objdump"))
            .args(["-d", "--no-print-imm-hex", "--triple=thumbv7m-none-eabi"])
            .args(triple_flags)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        let mut it = ItState::default();
        let mut next = None;
        // After an UNPREDICTABLE IT (inside a block, or AL with an else)
        // LLVM prints <und> conditions and its block tracking drifts from
        // the architecture's: skip a few instructions, then start clean.
        let mut skip = 0u32;
        let mut skip_block = false;
        for line in text.lines() {
            let Some((addr, rest)) = line.trim_start().split_once(':') else {
                continue;
            };
            let Ok(addr) = u32::from_str_radix(addr, 16) else {
                continue;
            };
            let Some((raw, asm)) = rest.split_once('\t') else {
                continue;
            };
            let hws: Vec<u16> = raw
                .split_whitespace()
                .map(|h| u16::from_str_radix(h, 16))
                .collect::<Result<_, _>>()
                .unwrap_or_default();
            let asm = asm.replace('\t', " ");
            if asm.trim_start().starts_with('.')
                || hws.is_empty()
                || raw.split_whitespace().any(|h| h.len() != 4)
            {
                it = ItState::default(); // data
                next = None;
                continue;
            }
            if next != Some(addr) {
                it = ItState::default();
            }
            let insn = decode(hws[0], hws.get(1).copied().unwrap_or(0), it.in_block());
            next = Some(addr + insn.len as u32);
            if insn.len as usize != hws.len() * 2 {
                self.mismatches
                    .entry(format!("length {}", normalize(&asm)))
                    .or_default()
                    .push(format!("{what} {addr:08x}: {raw:?}"));
                it = ItState::default();
                continue;
            }
            if let Op::It { firstcond, mask } = insn.op {
                let al_else = firstcond == 14 && it_has_else(firstcond, mask);
                if it.in_block() || firstcond == 15 || al_else {
                    skip = 9;
                    skip_block = true;
                }
            }
            if skip > 0 || it.in_block() && skip_block {
                skip = skip.saturating_sub(1);
                self.unpredictable += 1;
                match insn.op {
                    Op::It { firstcond, mask } => it = ItState::start(firstcond, mask),
                    _ if it.in_block() => it.advance(),
                    _ => {}
                }
                skip_block = skip > 0 || it.in_block();
                continue;
            }
            let ours = normalize(&disasm(&insn, addr, it.cond()));
            let theirs = normalize(&asm);
            self.compared += 1;
            let theirs_mnem = theirs.split(' ').next().unwrap_or("");
            if ours == "<unknown>" && COPROCESSOR.iter().any(|m| theirs_mnem.starts_with(m)) {
                self.coprocessor += 1;
            } else if insn.unpredictable
                && (theirs == "<unknown>" || theirs_mnem == ours.split(' ').next().unwrap_or(""))
            {
                // Flagged UNPREDICTABLE: LLVM may reject it, or read the
                // same instruction with other operands.
                self.flagged += 1;
            } else if ours != theirs {
                let key = if theirs_mnem == "<unknown>" {
                    format!("<unknown> (ours {})", ours.split(' ').next().unwrap_or(""))
                } else {
                    theirs_mnem.to_string()
                };
                let v = self.mismatches.entry(key).or_default();
                if v.len() < 4 {
                    v.push(format!(
                        "{what} {addr:08x} {}: llvm `{theirs}` ours `{ours}`",
                        raw.trim()
                    ));
                } else {
                    v.push(String::new());
                }
            }
            match insn.op {
                Op::It { firstcond, mask } => it = ItState::start(firstcond, mask),
                _ if it.in_block() => it.advance(),
                _ => {}
            }
        }
    }

    fn finish(self) {
        let total: usize = self.mismatches.values().map(Vec::len).sum();
        let mut msg = format!(
            "{} instructions compared, {} coprocessor ops (<unknown> here), {} flagged UNPREDICTABLE, {} skipped after UNPREDICTABLE ITs, {} mismatches\n",
            self.compared, self.coprocessor, self.flagged, self.unpredictable, total
        );
        for (mnem, v) in &self.mismatches {
            msg.push_str(&format!("  {mnem}: {}\n", v.len()));
            for e in v.iter().filter(|e| !e.is_empty()) {
                msg.push_str(&format!("    {e}\n"));
            }
        }
        eprintln!("{msg}");
        assert!(self.mismatches.is_empty(), "{msg}");
    }
}

#[test]
fn images_match_llvm() {
    let mut r = Report::default();
    for image in Image::ALL {
        r.check(image.file_name(), &image.path(), &[]);
    }
    r.finish();
}

#[test]
fn rom_dump_matches_llvm() {
    let dump = std::env::var_os("ROM_WORK")
        .map(|w| PathBuf::from(w).join("dump/rom.bin"))
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../wb32-rom-re/dump/rom.bin")
        });
    let Ok(data) = std::fs::read(&dump) else {
        eprintln!("SKIPPED: no ROM dump at {} (set ROM_WORK)", dump.display());
        return;
    };
    let mut r = Report::default();
    r.check("rom", &blob_elf("rom", &data, 0x1FFF_E000), &[]);
    r.finish();
}

#[test]
fn random_blob_matches_llvm() {
    // xorshift32, fixed seed: the same blob every run.
    let mut x: u32 = 0x2545_F491;
    let mut data = Vec::with_capacity(800_000);
    for _ in 0..400_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        data.extend_from_slice(&(x as u16).to_le_bytes());
    }
    let mut r = Report::default();
    r.check("random", &blob_elf("random", &data, 0x0800_0000), &[]);
    r.finish();
}
