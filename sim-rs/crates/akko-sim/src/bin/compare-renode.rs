//! Runs an image on our core in lockstep with a Renode trace of the same
//! image (compare/renode-trace.sh), instruction by instruction:
//! - every peripheral read gets the value Renode's model returned, and
//!   every peripheral write must match Renode's, in order (replay: this
//!   checks the core, not our peripheral models);
//! - asynchronous exceptions are taken where Renode took them (a jump to a
//!   handler the code didn't branch to), and the tool notes whether our
//!   NVIC had that exception pending at the time;
//! - every executed instruction's PC and opcode must match;
//! - Renode doesn't log reads of the core's timers and pending state
//!   (SysTick, ICSR, NVIC pending, CYCCNT), and its timing differs from
//!   ours within a block, so after such a read the registers come from a
//!   snapshot Renode took at the next instruction (renode-trace.sh hooks
//!   the PCs in <image>.hooks). A snapshot also checks every other
//!   register. A read with no snapshot adds its next PC to the hooks file
//!   and exits with 2: run renode-trace.sh again.
//!
//! Stops at the first difference and shows the instructions before it.
//!   compare-renode <image> [max instructions]
//! <image> "rom" is the ROM bootloader from $ROM_WORK/dump (default
//! ../wb32-rom-re beside the repo), entered as rom/sim/rom.resc does.

use akko_sim::{Board, Image};
use cortex_m3_emu::{Bus, CoreDiag, IrqLines, Size, Sleep, decode, disasm};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use wb32_sim::Wb32;

/// Renode peripheral names (sim/akko.resc, rom/sim/rom.resc) and bases.
const PERIPHERALS: [(&str, u32); 10] = [
    ("gpio", 0x4000_0000),
    ("sysctl", 0x4001_0000),
    ("cache", 0x4001_5400),
    ("usb", 0x4001_4000),
    ("qspi", 0x4000_3000),
    ("spim2", 0x4000_9000),
    ("uart3", 0x4000_8400),
    ("tim2", 0x4000_2000),
    ("sys", 0x4001_6400),
    ("fmc", 0x4001_7800),
];

/// What sim/peripherals/usb.py reads through the bus every millisecond to
/// decide when to attach (GPIOA MODER, RCC USBPRE/AHBENR1/USBCLKENR/
/// USBFIFOCLKENR): the model's accesses, not the CPU's.
/// Core registers whose value depends on timing: SysTick CSR and CVR,
/// ICSR, NVIC set/clear-pending, DWT CYCCNT.
fn time_dependent(addr: u32) -> bool {
    matches!(addr, 0xE000_E010 | 0xE000_E018 | 0xE000_ED04 | 0xE000_1004)
        || (0xE000_E200..0xE000_E300).contains(&addr)
}

const MODEL_POLLS: [u32; 5] = [0x4000_0000, 0x4001_0C14, 0x4001_0C40, 0x4001_0C6C, 0x4001_0C7C];

#[derive(Clone, Debug)]
struct Access {
    addr: u32,
    size: Size,
    write: bool,
    value: u32,
    /// Renode's PC for the access (its translation block's, roughly).
    pc: u32,
}

fn parse_log(text: &str) -> Vec<Access> {
    let mut out = Vec::new();
    for line in text.lines() {
        // [time] [LEVEL] name: [cpu: 0xPC] ReadUInt32 from 0xOFF (..), returned 0xV
        let Some(rest) = line.splitn(3, "] ").nth(2) else { continue };
        let Some((name, rest)) = rest.split_once(": [cpu: ") else { continue };
        let Some(&(_, base)) = PERIPHERALS.iter().find(|p| p.0 == name) else { continue };
        let Some((pc, rest)) = rest.split_once("] ") else { continue };
        let mut words = rest.split_whitespace();
        let op = words.next().unwrap_or("");
        let (write, size) = match op {
            "ReadByte" => (false, Size::Byte),
            "ReadUInt16" => (false, Size::Half),
            "ReadUInt32" => (false, Size::Word),
            "WriteByte" => (true, Size::Byte),
            "WriteUInt16" => (true, Size::Half),
            "WriteUInt32" => (true, Size::Word),
            _ => continue,
        };
        let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x").trim_end_matches(','), 16).unwrap();
        let off = hex(words.nth(1).unwrap());
        let value = hex(rest.rsplit_once(" 0x").map(|(_, v)| v).unwrap().split_whitespace().next().unwrap());
        // Renode writes a run of identical lines as the line, then the line
        // again with " (N)" for the N repeats after the first.
        let repeats = match rest.rsplit_once(" (") {
            Some((_, n)) if n.ends_with(')') => n.trim_end_matches(')').parse::<usize>().unwrap(),
            _ => 1,
        };
        let a = Access { addr: base + off, size, write, value, pc: hex(pc) };
        out.extend(std::iter::repeat_n(a, repeats));
    }
    out
}

/// `[time] [INFO] cpu: snap 0xPC r0 .. r12` lines (renode-trace.sh's hooks),
/// by PC in the order they were taken.
fn parse_snaps(text: &str) -> BTreeMap<u32, VecDeque<[u32; 13]>> {
    let mut out: BTreeMap<u32, VecDeque<[u32; 13]>> = BTreeMap::new();
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x").trim_end_matches('L'), 16).unwrap();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("cpu: snap ") else { continue };
        let v: Vec<u32> = rest.split_whitespace().map(hex).collect();
        if v.len() == 14 {
            out.entry(v[0]).or_default().push_back(v[1..].try_into().unwrap());
        }
    }
    out
}

fn parse_trace(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let (pc, op) = l.split_once(": ")?;
            let h = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
            Some((h(pc)?, h(op)?))
        })
        .collect()
}

struct Replay {
    chip: Wb32,
    accesses: VecDeque<Access>,
    done: usize,
    model_polls: usize,
    error: Option<String>,
}

impl Replay {
    fn periph(addr: u32) -> bool {
        (0x4000_0000..0x4002_0000).contains(&addr)
    }

    fn next(&mut self, addr: u32, size: Size, write: Option<u32>) -> u32 {
        // Skip the USB model's own polls unless they're this access.
        while let Some(a) = self.accesses.front() {
            let is_this = a.addr == addr && a.write == write.is_some();
            if is_this || a.write || !MODEL_POLLS.contains(&a.addr) {
                break;
            }
            self.accesses.pop_front();
            self.model_polls += 1;
        }
        let Some(a) = self.accesses.pop_front() else {
            self.error.get_or_insert_with(|| format!("access 0x{addr:08x} beyond Renode's log"));
            return 0;
        };
        self.done += 1;
        let ok = a.addr == addr && a.size == size && a.write == write.is_some() && write.is_none_or(|v| v == a.value);
        if !ok && self.error.is_none() {
            let ours = match write {
                Some(v) => format!("write{} 0x{addr:08x} = 0x{v:x}", size.bytes() * 8),
                None => format!("read{} 0x{addr:08x}", size.bytes() * 8),
            };
            let theirs = format!(
                "{}{} 0x{:08x} {} 0x{:x} (Renode PC ~0x{:08x})",
                if a.write { "write" } else { "read" },
                a.size.bytes() * 8,
                a.addr,
                if a.write { "=" } else { "->" },
                a.value,
                a.pc
            );
            self.error = Some(format!("access #{}: ours {ours}, Renode {theirs}", self.done));
        }
        a.value
    }
}

impl Bus for Replay {
    fn read(&mut self, addr: u32, size: Size) -> u32 {
        if Self::periph(addr) { self.next(addr, size, None) } else { self.chip.read(addr, size) }
    }
    fn write(&mut self, addr: u32, size: Size, value: u32) {
        if Self::periph(addr) {
            self.next(addr, size, Some(value));
        } else {
            self.chip.write(addr, size, value)
        }
    }
    fn fetch(&mut self, addr: u32) -> Option<u16> {
        self.chip.fetch(addr)
    }
    fn irq_lines(&mut self) -> IrqLines {
        IrqLines::default()
    }
}

fn image_named(name: &str) -> Image {
    *Image::ALL.iter().find(|i| i.file_name() == name).unwrap_or_else(|| panic!("no image {name}"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let name = args.get(1).expect("usage: compare-renode <image> [max]");
    let max: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(usize::MAX);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/renode");
    let trace = parse_trace(&std::fs::read_to_string(dir.join(format!("{name}.trace"))).expect("trace"));
    let log = std::fs::read_to_string(dir.join(format!("{name}.log"))).expect("log");
    let accesses = parse_log(&log);
    let mut snaps = parse_snaps(&log);
    let hooks_file = dir.join(format!("{name}.hooks"));
    let mut hooks: std::collections::BTreeSet<u32> = std::fs::read_to_string(&hooks_file)
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
        .collect();
    let hooks_before = hooks.len();
    let mut patched = 0;
    let mut checked = 0;
    println!("Renode: {} instructions, {} peripheral accesses", trace.len(), accesses.len());

    let mut board = Board::new();
    if name != "rom" {
        board.load(image_named(name)).unwrap();
    }
    let Board { mut cpu, chip, .. } = board;
    let mut bus = Replay { chip, accesses: accesses.into(), done: 0, model_polls: 0, error: None };
    cpu.lockstep = true;
    cpu.reset(&mut bus);
    if name == "rom" {
        // rom.resc's enter_rom: the dump over the ROM stub, then boot-guard's jump.
        let work = std::env::var_os("ROM_WORK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../wb32-rom-re"));
        for (file, addr) in [("rom.bin", 0x1FFF_E000), ("info.bin", 0x1FFF_F000)] {
            let bytes = std::fs::read(work.join("dump").join(file)).expect("ROM dump: see rom/README.md");
            bus.chip.load(addr, &bytes).unwrap();
        }
        cpu.r[13] = 0x2000_0450;
        cpu.r[15] = 0x1FFF_E090;
        cpu.thumb = true;
    }

    let mut injected: BTreeMap<u16, (usize, usize)> = BTreeMap::new(); // exc -> (taken, ours pending)
    let mut recent: VecDeque<String> = VecDeque::new();
    let mut diags_seen = 0;
    let mut time_read = false;
    let n = trace.len().min(max);
    for (i, &(pc_r, op_r)) in trace.iter().take(n).enumerate() {
        if cpu.pc() != pc_r {
            // Renode took an exception here?
            let vtor = cpu.ppb.vtor;
            let candidates: Vec<u16> = (2..80u16)
                .filter(|&e| bus.chip.read(vtor + 4 * e as u32, Size::Word) & !1 == pc_r)
                .collect();
            if candidates.is_empty() {
                fail(i, &recent, (&hooks_file, &hooks, hooks_before), &format!("PC: ours 0x{:08x}, Renode 0x{pc_r:08x}", cpu.pc()), &trace);
            }
            // Asleep, ours would have jumped to its next event: if that's
            // SysTick, it should be pending now.
            if cpu.sleep != Sleep::Awake && candidates.contains(&15) {
                let t = cpu.ppb.next_event();
                if t != u64::MAX && t > cpu.now {
                    cpu.now = t;
                    cpu.ppb.tick(t);
                }
            }
            let pending = |e: u16| cpu.ppb.pending & (1 << e) != 0;
            let exc = candidates.iter().copied().find(|&e| pending(e)).unwrap_or(candidates[0]);
            let entry = injected.entry(exc).or_default();
            entry.0 += 1;
            if pending(exc) {
                entry.1 += 1;
            }
            recent.push_back(format!("        -- exception {exc} (ours pending: {})", pending(exc)));
            cpu.take_exception(&mut bus, exc);
        } else if cpu.sleep != Sleep::Awake {
            cpu.sleep = Sleep::Awake; // woke without taking anything
        }
        if let Some(q) = snaps.get_mut(&pc_r) {
            let Some(snap) = q.pop_front() else {
                fail(i, &recent, (&hooks_file, &hooks, hooks_before), "Renode's snapshots at this PC ran out", &trace);
            };
            checked += 1;
            for (r, &v) in snap.iter().enumerate() {
                if cpu.r[r] == v {
                    continue;
                }
                if !time_read {
                    fail(i, &recent, (&hooks_file, &hooks, hooks_before), &format!("r{r}: ours 0x{:x}, Renode 0x{v:x}", cpu.r[r]), &trace);
                }
                recent.push_back(format!("        -- r{r} = 0x{v:x} from Renode (ours 0x{:x})", cpu.r[r]));
                cpu.r[r] = v;
                patched += 1;
            }
        } else if time_read {
            hooks.insert(pc_r);
        }
        let hw1 = bus.chip.fetch(pc_r).unwrap_or(0);
        let hw2 = bus.chip.fetch(pc_r + 2).unwrap_or(0);
        let insn = decode(hw1, hw2, cpu.it.in_block());
        let op = if insn.len == 4 { (hw1 as u32) << 16 | hw2 as u32 } else { hw1 as u32 };
        if op != op_r {
            fail(i, &recent, (&hooks_file, &hooks, hooks_before), &format!("opcode at 0x{pc_r:08x}: ours 0x{op:x}, Renode 0x{op_r:x}"), &trace);
        }
        recent.push_back(format!("{i:>8} {pc_r:08x}: {}", disasm(&insn, pc_r, cpu.it.cond())));
        if recent.len() > 24 {
            recent.pop_front();
        }
        cpu.ppb_reads.clear();
        cpu.step(&mut bus);
        time_read = cpu.ppb_reads.iter().any(|&a| time_dependent(a));
        if let Some(e) = bus.error.take() {
            fail(i, &recent, (&hooks_file, &hooks, hooks_before), &e, &trace);
        }
        for d in &cpu.diags[diags_seen..] {
            if !matches!(d, CoreDiag::Ppb { .. }) {
                fail(i, &recent, (&hooks_file, &hooks, hooks_before), &format!("core: {d}"), &trace);
            }
            println!("note: core: {d}");
        }
        diags_seen = cpu.diags.len();
    }
    if hooks.len() > hooks_before {
        save_hooks(&hooks_file, &hooks);
    }
    println!(
        "match: {n} instructions, {} peripheral accesses replayed ({} USB model polls skipped)",
        bus.done, bus.model_polls
    );
    bus.accesses.retain(|a| a.write || !MODEL_POLLS.contains(&a.addr));
    if !bus.accesses.is_empty() {
        let a = &bus.accesses[0];
        println!(
            "Renode's log has {} more (the first: {} 0x{:08x} at Renode PC ~0x{:08x})",
            bus.accesses.len(),
            if a.write { "write" } else { "read" },
            a.addr,
            a.pc
        );
    }
    println!("{checked} register snapshots checked, {patched} time-dependent registers taken from Renode");
    for (exc, (taken, pending)) in injected {
        println!("exception {exc}: taken {taken} times where Renode took it; ours had it pending {pending} of those");
    }
    for d in bus.chip.take_diags().iter().take(5) {
        println!("chip: {d}");
    }
}

fn save_hooks(path: &std::path::Path, hooks: &std::collections::BTreeSet<u32>) {
    let text: String = hooks.iter().map(|h| format!("0x{h:08x}\n")).collect();
    std::fs::write(path, text).unwrap();
}

type Hooks<'a> = (&'a std::path::Path, &'a std::collections::BTreeSet<u32>, usize);

fn fail(i: usize, recent: &VecDeque<String>, (file, hooks, before): Hooks, what: &str, trace: &[(u32, u32)]) -> ! {
    if hooks.len() > before {
        save_hooks(file, hooks);
        println!(
            "{} new time-dependent reads before instruction {i}: {} lists them; run renode-trace.sh again",
            hooks.len() - before,
            file.display()
        );
        std::process::exit(2);
    }
    for l in recent {
        println!("{l}");
    }
    let next: Vec<String> = trace[i..trace.len().min(i + 4)].iter().map(|(p, _)| format!("0x{p:08x}")).collect();
    println!("DIFFERENT at instruction {i}: {what}\n  Renode continues {}", next.join(" "));
    std::process::exit(1);
}
