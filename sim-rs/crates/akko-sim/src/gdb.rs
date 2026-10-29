//! A GDB remote stub on the board (`akko-sim gdb`), as a debug probe's
//! (OpenOCD, probe-rs) is to the keyboard: `target remote :3333`, then
//! breakpoints, steps, registers and memory. The protocol is the GDB
//! manual's "Remote Protocol" appendix; the registers are GDB's M-profile
//! features (org.gnu.gdb.arm.m-profile, m-system) and the special
//! registers. The core halts between instructions, and emulated time
//! stands still while it's halted; running, it goes as fast as it can.

use crate::Board;
use cortex_m3_emu::{Size, TICK_HZ};
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Emulated time between looks for GDB's interrupt (Ctrl-C) while running.
const SLICE: f64 = 0.001;

// GDB's signal numbers, for stop replies.
const SIGINT: u8 = 2;
const SIGTRAP: u8 = 5;
const SIGSEGV: u8 = 11;

/// The registers, in `g` packet order: (name, GDB type, feature).
const REGS: [(&str, &str, &str); 23] = [
    ("r0", "uint32", "m-profile"),
    ("r1", "uint32", "m-profile"),
    ("r2", "uint32", "m-profile"),
    ("r3", "uint32", "m-profile"),
    ("r4", "uint32", "m-profile"),
    ("r5", "uint32", "m-profile"),
    ("r6", "uint32", "m-profile"),
    ("r7", "uint32", "m-profile"),
    ("r8", "uint32", "m-profile"),
    ("r9", "uint32", "m-profile"),
    ("r10", "uint32", "m-profile"),
    ("r11", "uint32", "m-profile"),
    ("r12", "uint32", "m-profile"),
    ("sp", "data_ptr", "m-profile"),
    ("lr", "uint32", "m-profile"),
    ("pc", "code_ptr", "m-profile"),
    ("xpsr", "uint32", "m-profile"),
    ("msp", "data_ptr", "m-system"),
    ("psp", "data_ptr", "m-system"),
    ("primask", "uint32", "special"),
    ("basepri", "uint32", "special"),
    ("faultmask", "uint32", "special"),
    ("control", "uint32", "special"),
];
const XPSR: usize = 16;

fn target_xml() -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE target SYSTEM \"gdb-target.dtd\">\n\
         <target version=\"1.0\">\n<architecture>arm</architecture>\n",
    );
    for feature in ["m-profile", "m-system", "special"] {
        let name = match feature {
            "special" => "org.akko-sim.arm.m-special".to_string(),
            f => format!("org.gnu.gdb.arm.{f}"),
        };
        x += &format!("<feature name=\"{name}\">\n");
        for (i, (reg, ty, _)) in REGS.iter().enumerate().filter(|r| r.1.2 == feature) {
            x += &format!("<reg name=\"{reg}\" bitsize=\"32\" type=\"{ty}\" regnum=\"{i}\"/>\n");
        }
        x += "</feature>\n";
    }
    x + "</target>\n"
}

/// What a packet asks of the connection.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Packet(String),
    /// Run (`Gdb::resume`), then send the stop reply.
    Resume(Resume),
    /// Answer OK and close.
    Detach,
    /// Close without an answer.
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    Continue,
    Step,
}

pub struct Gdb {
    pub board: Board,
    breaks: HashSet<u32>,
    /// The signal of the last stop.
    stop: u8,
}

impl Gdb {
    /// Attaches to `board`, halted where it is. A BKPT halts the core from
    /// now on (`Cpu::debugger`).
    pub fn new(mut board: Board) -> Gdb {
        board.cpu.debugger = true;
        Gdb {
            board,
            breaks: HashSet::new(),
            stop: SIGTRAP,
        }
    }

    pub fn stop_reply(&self) -> String {
        format!("S{:02x}", self.stop)
    }

    /// Answers one packet (its payload, unescaped).
    pub fn packet(&mut self, p: &str) -> Reply {
        let reply = |s: &str| Reply::Packet(s.to_string());
        if let Some(args) = p.strip_prefix("qXfer:features:read:target.xml:") {
            return Reply::Packet(match parse_pair(args, ',') {
                Some((off, len)) => chunk(&target_xml(), off as usize, len as usize),
                None => "E01".into(),
            });
        }
        if let Some(hex) = p.strip_prefix("qRcmd,") {
            let cmd = unhex(hex).map(|b| String::from_utf8_lossy(&b).into_owned());
            return Reply::Packet(match cmd {
                Some(cmd) => hex_bytes(self.monitor(cmd.trim()).as_bytes()),
                None => "E01".into(),
            });
        }
        if let Some(actions) = p.strip_prefix("vCont;") {
            // One thread: the first action decides.
            return match actions.as_bytes().first() {
                Some(b'c' | b'C') => Reply::Resume(Resume::Continue),
                Some(b's' | b'S') => Reply::Resume(Resume::Step),
                _ => reply("E01"),
            };
        }
        match p {
            "?" => return Reply::Packet(self.stop_reply()),
            "QStartNoAckMode" => return reply("OK"),
            "qAttached" => return reply("1"),
            "qC" => return reply("QC1"),
            "qfThreadInfo" => return reply("m1"),
            "qsThreadInfo" => return reply("l"),
            "vCont?" => return reply("vCont;c;C;s;S"),
            "g" => return Reply::Packet((0..REGS.len()).map(|i| hex_u32(self.reg(i))).collect()),
            "D" => return Reply::Detach,
            "k" | "vKill;1" => return Reply::Kill,
            _ => {}
        }
        if p.starts_with("qSupported") {
            return reply("PacketSize=4000;qXfer:features:read+;QStartNoAckMode+;vContSupported+");
        }
        let (head, rest) = p.split_at(p.chars().next().map_or(0, char::len_utf8));
        Reply::Packet(match head {
            "H" | "T" => "OK".into(),
            "G" => self.write_regs(rest),
            "p" => match usize::from_str_radix(rest, 16) {
                Ok(i) if i < REGS.len() => hex_u32(self.reg(i)),
                _ => "E01".into(),
            },
            "P" => match rest
                .split_once('=')
                .and_then(|(n, v)| Some((usize::from_str_radix(n, 16).ok()?, le_u32(v)?)))
            {
                Some((i, v)) if i < REGS.len() => {
                    self.set_reg(i, v);
                    "OK".into()
                }
                _ => "E01".into(),
            },
            "m" => match parse_pair(rest, ',') {
                Some((addr, len)) => self.read_mem(addr, len),
                None => "E01".into(),
            },
            "M" => match rest
                .split_once(':')
                .and_then(|(a, d)| Some((parse_pair(a, ',')?, unhex(d)?)))
            {
                Some(((addr, len), data)) if data.len() == len as usize => {
                    self.write_mem(addr, &data)
                }
                _ => "E01".into(),
            },
            "Z" | "z" => self.breakpoint(head == "Z", rest),
            "c" | "s" | "C" | "S" => {
                // c/s [addr]: resume there; C/S sig[;addr]: the signal is
                // ignored (there's no OS to deliver it to).
                let addr = match head {
                    "c" | "s" => rest,
                    _ => rest.split_once(';').map_or("", |(_, a)| a),
                };
                if let Ok(a) = u32::from_str_radix(addr, 16) {
                    self.board.cpu.r[15] = a & !1;
                }
                return Reply::Resume(if matches!(head, "c" | "C") {
                    Resume::Continue
                } else {
                    Resume::Step
                });
            }
            _ => String::new(),
        })
    }

    /// Runs (or steps), looking at `interrupted` every SLICE of emulated
    /// time; the stop reply.
    pub fn resume(&mut self, how: Resume, mut interrupted: impl FnMut() -> bool) -> String {
        self.stop = loop {
            let done = match how {
                Resume::Continue => self.board.run_for_break(SLICE, &self.breaks),
                Resume::Step => self.board.debug_step(SLICE),
            };
            if self.board.cpu.halted {
                break SIGSEGV; // locked up
            }
            if done {
                break SIGTRAP;
            }
            if interrupted() {
                break SIGINT;
            }
        };
        self.stop_reply()
    }

    fn reg(&self, i: usize) -> u32 {
        let c = &self.board.cpu;
        match i {
            0..=15 => c.r[i],
            XPSR => c.xpsr(),
            17 => c.msp(),
            18 => c.psp(),
            19 => c.primask as u32,
            20 => c.basepri as u32,
            21 => c.faultmask as u32,
            _ => c.control as u32,
        }
    }

    fn set_reg(&mut self, i: usize, v: u32) {
        let c = &mut self.board.cpu;
        match i {
            0..=14 => c.r[i] = v,
            15 => c.r[15] = v & !1,
            XPSR => {
                // The flags, T and the IT bits; IPSR is the exception's.
                c.apsr = v & 0xF800_0000;
                c.thumb = v & 1 << 24 != 0;
                c.it.0 = ((v >> 25) & 3 | (v >> 10 & 0x3F) << 2) as u8;
            }
            17 => c.set_msp(v),
            18 => c.set_psp(v),
            19 => c.primask = v & 1 != 0,
            20 => c.basepri = v as u8,
            21 => c.faultmask = v & 1 != 0,
            _ => c.set_control(v as u8),
        }
    }

    fn write_regs(&mut self, hex: &str) -> String {
        if hex.len() != REGS.len() * 8 {
            return "E01".into();
        }
        let mut vals = Vec::new();
        for i in 0..REGS.len() {
            match le_u32(&hex[i * 8..i * 8 + 8]) {
                Some(v) => vals.push(v),
                None => return "E01".into(),
            }
        }
        for (i, v) in vals.into_iter().enumerate() {
            self.set_reg(i, v);
        }
        "OK".into()
    }

    /// Word accesses where aligned (peripheral registers want them), bytes
    /// at the edges. Stops at the first address nothing answers; E14 if
    /// that's the first.
    fn read_mem(&mut self, addr: u32, len: u32) -> String {
        let mut out = Vec::new();
        let mut a = addr;
        let end = addr.wrapping_add(len);
        while a != end {
            let size = if a % 4 == 0 && end.wrapping_sub(a) >= 4 {
                Size::Word
            } else {
                Size::Byte
            };
            let Some(v) = self.board.debug_read(a, size) else {
                break;
            };
            out.extend_from_slice(&v.to_le_bytes()[..size.bytes() as usize]);
            a = a.wrapping_add(size.bytes());
        }
        if out.is_empty() && len != 0 {
            return "E14".into();
        }
        hex_bytes(&out)
    }

    fn write_mem(&mut self, addr: u32, data: &[u8]) -> String {
        let mut i = 0;
        while i < data.len() {
            let a = addr.wrapping_add(i as u32);
            let (size, v) = if a % 4 == 0 && data.len() - i >= 4 {
                let w: [u8; 4] = data[i..i + 4].try_into().unwrap();
                (Size::Word, u32::from_le_bytes(w))
            } else {
                (Size::Byte, data[i] as u32)
            };
            if !self.board.debug_write(a, size, v) {
                return "E14".into();
            }
            i += size.bytes() as usize;
        }
        "OK".into()
    }

    /// Z/z type,addr,kind: software and hardware breakpoints alike (the
    /// core stops before the address either way). Watchpoints aren't
    /// supported: an empty reply says so.
    fn breakpoint(&mut self, insert: bool, args: &str) -> String {
        let mut it = args.splitn(3, ',');
        let (Some(ty), Some(addr)) = (it.next(), it.next()) else {
            return "E01".into();
        };
        if !matches!(ty, "0" | "1") {
            return String::new();
        }
        let Ok(addr) = u32::from_str_radix(addr, 16) else {
            return "E01".into();
        };
        if insert {
            self.breaks.insert(addr & !1);
        } else {
            self.breaks.remove(&(addr & !1));
        }
        "OK".into()
    }

    /// `monitor <cmd>`: the output.
    fn monitor(&mut self, cmd: &str) -> String {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        match words[..] {
            ["reset"] => {
                self.board.system_reset();
                "system reset (SRAM kept), halted at the reset vector\n".into()
            }
            ["time"] => format!(
                "{:.6} s emulated, {} instructions\n",
                self.board.cpu.now as f64 / TICK_HZ as f64,
                self.board.cpu.instructions
            ),
            ["key", row, col, state @ ("down" | "up")] => {
                match (row.parse::<u8>(), col.parse::<u8>()) {
                    (Ok(r), Ok(c)) => {
                        self.board.key(r, c, state == "down");
                        format!("key {r},{c} {state}\n")
                    }
                    _ => "key <row> <col> down|up\n".into(),
                }
            }
            _ => "monitor commands: reset, time, key <row> <col> down|up\n".into(),
        }
    }
}

/// Serves one GDB connection until it detaches, kills or closes.
pub fn serve(gdb: &mut Gdb, stream: TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut conn = Conn {
        stream,
        buf: Vec::new(),
        ack: true,
    };
    while let Some(p) = conn.packet()? {
        let reply = match gdb.packet(&p) {
            Reply::Packet(r) => r,
            Reply::Resume(how) => gdb.resume(how, || conn.interrupted()),
            Reply::Detach => return conn.send("OK"),
            Reply::Kill => return Ok(()),
        };
        conn.send(&reply)?;
        if p == "QStartNoAckMode" {
            conn.ack = false;
        }
    }
    Ok(())
}

struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
    ack: bool,
}

impl Conn {
    /// Reads more; false at the end of the stream.
    fn fill(&mut self) -> io::Result<bool> {
        let mut tmp = [0; 4096];
        let n = self.stream.read(&mut tmp)?;
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(n != 0)
    }

    /// The next packet's payload, acknowledged; None at the end of the
    /// stream. Acks and interrupts before it are dropped (the core is
    /// already halted).
    fn packet(&mut self) -> io::Result<Option<String>> {
        loop {
            let start = self.buf.iter().position(|&b| b == b'$');
            self.buf.drain(..start.unwrap_or(self.buf.len()));
            if let Some(hash) = self.buf.iter().position(|&b| b == b'#')
                && self.buf.len() >= hash + 3
            {
                let body = &self.buf[1..hash];
                let sum = body.iter().fold(0u8, |s, &b| s.wrapping_add(b));
                let ok = std::str::from_utf8(&self.buf[hash + 1..hash + 3])
                    .ok()
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
                    == Some(sum);
                let mut data = Vec::with_capacity(body.len());
                let mut esc = false;
                for &b in body {
                    match (esc, b) {
                        (false, b'}') => esc = true,
                        (true, b) => {
                            data.push(b ^ 0x20);
                            esc = false;
                        }
                        (false, b) => data.push(b),
                    }
                }
                self.buf.drain(..hash + 3);
                if self.ack {
                    self.stream.write_all(if ok { b"+" } else { b"-" })?;
                }
                if ok {
                    return Ok(Some(String::from_utf8_lossy(&data).into_owned()));
                }
                continue;
            }
            if !self.fill()? {
                return Ok(None);
            }
        }
    }

    /// While running: whether GDB sent its interrupt (0x03), or went away.
    fn interrupted(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return true;
        }
        let gone = match self.fill() {
            Ok(more) => !more,
            Err(e) => e.kind() != io::ErrorKind::WouldBlock,
        };
        let _ = self.stream.set_nonblocking(false);
        if let Some(i) = self.buf.iter().position(|&b| b == 0x03) {
            self.buf.remove(i);
            return true;
        }
        gone
    }

    fn send(&mut self, body: &str) -> io::Result<()> {
        let mut out = Vec::with_capacity(body.len() + 4);
        out.push(b'$');
        let mut sum = 0u8;
        for &b in body.as_bytes() {
            let esc: &[u8] = if matches!(b, b'#' | b'$' | b'}' | b'*') {
                &[b'}', b ^ 0x20]
            } else {
                std::slice::from_ref(&b)
            };
            for &e in esc {
                sum = sum.wrapping_add(e);
                out.push(e);
            }
        }
        out.extend_from_slice(format!("#{sum:02x}").as_bytes());
        self.stream.write_all(&out)
    }
}

/// "off,len" (or "addr,len") in hex.
fn parse_pair(s: &str, sep: char) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(sep)?;
    Some((
        u32::from_str_radix(a, 16).ok()?,
        u32::from_str_radix(b, 16).ok()?,
    ))
}

/// A qXfer reply: "m" and a piece, or "l" and the last.
fn chunk(doc: &str, off: usize, len: usize) -> String {
    let rest = doc.get(off.min(doc.len())..).unwrap_or("");
    if rest.len() > len {
        format!("m{}", &rest[..len])
    } else {
        format!("l{rest}")
    }
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// A register as the target's bytes (little-endian) in hex.
fn hex_u32(v: u32) -> String {
    hex_bytes(&v.to_le_bytes())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn le_u32(s: &str) -> Option<u32> {
    Some(u32::from_le_bytes(unhex(s)?.try_into().ok()?))
}
