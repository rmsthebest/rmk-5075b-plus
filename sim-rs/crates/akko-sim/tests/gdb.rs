//! The GDB stub (akko_sim::gdb): its packets against the board, the
//! interrupt over a socket, and a session with GDB itself (`gdb` on PATH,
//! or $GDB), the protocol's reference.

use akko_sim::gdb::{Gdb, Reply, Resume, serve};
use akko_sim::{Board, Image};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

fn attach(image: Image) -> Gdb {
    Gdb::new(Board::boot(image).unwrap())
}

fn ask(g: &mut Gdb, p: &str) -> String {
    match g.packet(p) {
        Reply::Packet(r) => r,
        r => panic!("{p}: {r:?}"),
    }
}

fn reg(g: &mut Gdb, n: usize) -> u32 {
    let hex = ask(g, &format!("p{n:x}"));
    u32::from_str_radix(&hex, 16).unwrap().swap_bytes()
}

/// The first word of a memory read.
fn word(g: &mut Gdb, addr: u32) -> u32 {
    let hex = ask(g, &format!("m{addr:x},4"));
    u32::from_str_radix(&hex, 16).unwrap().swap_bytes()
}

fn resume(g: &mut Gdb, p: &str) -> String {
    match g.packet(p) {
        Reply::Resume(how) => g.resume(how, || false),
        r => panic!("{p}: {r:?}"),
    }
}

#[test]
fn registers_memory_and_the_target_description() {
    let mut g = attach(Image::M2Blink);
    assert_eq!(ask(&mut g, "?"), "S05");
    assert!(ask(&mut g, "qSupported:multiprocess+;swbreak+").contains("qXfer:features:read+"));
    // The description in pieces, as GDB reads it.
    let mut xml = String::new();
    loop {
        let r = ask(
            &mut g,
            &format!("qXfer:features:read:target.xml:{:x},40", xml.len()),
        );
        xml += &r[1..];
        if r.starts_with('l') {
            break;
        }
    }
    assert!(xml.contains("<feature name=\"org.gnu.gdb.arm.m-profile\">"));
    assert!(xml.contains("<reg name=\"xpsr\" bitsize=\"32\" type=\"uint32\" regnum=\"16\"/>"));
    assert!(xml.contains("<reg name=\"control\""));
    // At the reset vector: the vector table's SP and PC.
    assert_eq!(ask(&mut g, "g").len(), 23 * 8);
    assert_eq!(reg(&mut g, 13), word(&mut g, 0x0800_0000));
    assert_eq!(reg(&mut g, 15), word(&mut g, 0x0800_0004) & !1);
    assert_eq!(reg(&mut g, 16), 1 << 24, "xPSR: Thumb");
    assert_eq!(reg(&mut g, 17), reg(&mut g, 13), "MSP in use");
    assert_eq!(ask(&mut g, "P0=78563412"), "OK");
    assert_eq!(g.board.cpu.r[0], 0x1234_5678);
    // SRAM round trip, unaligned at both ends.
    assert_eq!(ask(&mut g, "M20000101,6:0102030405ff"), "OK");
    assert_eq!(ask(&mut g, "m20000100,8"), "000102030405ff00");
    // A peripheral register (GPIOA MODER's reset value is QMK's board's
    // concern; it reads as a word) and nothing at all.
    assert_eq!(ask(&mut g, "m40010800,4").len(), 8);
    assert_eq!(ask(&mut g, "m30000000,4"), "E14");
    // A debugger writes flash directly (`load`).
    assert_eq!(ask(&mut g, "M08000000,4:efbeadde"), "OK");
    assert_eq!(word(&mut g, 0x0800_0000), 0xDEAD_BEEF);
    assert_eq!(ask(&mut g, "Z2,20000000,4"), "", "no watchpoints");
    assert_eq!(ask(&mut g, "vMustReplyEmpty"), "");
}

#[test]
fn breakpoints_steps_and_reset() {
    let mut g = attach(Image::M2Blink);
    let reset = g.board.pc();
    let main = Image::M2Blink.exact_symbol("main").unwrap();
    assert_eq!(ask(&mut g, &format!("Z0,{main:x},2")), "OK");
    assert_eq!(resume(&mut g, "c"), "S05");
    assert_eq!(g.board.pc(), main);
    // A step from a breakpoint runs it: one instruction.
    let n = g.board.cpu.instructions;
    assert_eq!(resume(&mut g, "vCont;s:1"), "S05");
    assert_ne!(g.board.pc(), main);
    assert_eq!(g.board.cpu.instructions, n + 1);
    assert_eq!(ask(&mut g, &format!("z0,{main:x},2")), "OK");
    let out = ask(&mut g, &format!("qRcmd,{}", hex("reset")));
    assert!(
        String::from_utf8(unhex(&out))
            .unwrap()
            .starts_with("system reset")
    );
    assert_eq!(g.board.pc(), reset);
    // No breakpoint: m2 blinks on; time passes until the interrupt.
    let t = g.board.cpu.now;
    let mut slices = 0;
    let stop = match g.packet("c") {
        Reply::Resume(Resume::Continue) => g.resume(Resume::Continue, || {
            slices += 1;
            slices == 50
        }),
        r => panic!("{r:?}"),
    };
    assert_eq!(stop, "S02");
    assert_eq!(ask(&mut g, "?"), "S02");
    let ms = (g.board.cpu.now - t) as f64 / cortex_m3_emu::TICK_HZ as f64 * 1e3;
    assert!((49.9..50.1).contains(&ms), "{ms} ms");
    assert!(g.board.cpu.diags.is_empty(), "{:?}", g.board.cpu.diags);
}

#[test]
fn a_bkpt_stops_the_run() {
    // The breakpoint instruction itself, in SRAM: the core halts on it.
    let mut g = attach(Image::M2Blink);
    assert_eq!(ask(&mut g, "M20000200,2:00be"), "OK");
    assert_eq!(ask(&mut g, "P10=00000001"), "OK", "xPSR: Thumb");
    assert_eq!(resume(&mut g, "c20000200"), "S05");
    assert_eq!(g.board.pc(), 0x2000_0200);
    assert_eq!(g.board.cpu.ipsr, 0, "not a HardFault");
}

/// A packet as GDB frames it.
fn frame(p: &str) -> Vec<u8> {
    let sum = p.bytes().fold(0u8, |s, b| s.wrapping_add(b));
    format!("${p}#{sum:02x}").into_bytes()
}

/// Reads until `want` has come in.
fn expect(s: &mut TcpStream, want: &[u8]) {
    let mut got = Vec::new();
    let mut buf = [0; 256];
    while !got.windows(want.len()).any(|w| w == want) {
        let n = s.read(&mut buf).unwrap();
        assert_ne!(n, 0, "closed; got {:?}", String::from_utf8_lossy(&got));
        got.extend_from_slice(&buf[..n]);
    }
}

/// Serves one connection on a thread (the board isn't Send: it's built
/// there); the port.
fn server(image: Image) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = std::thread::spawn(move || {
        let mut g = attach(image);
        let (stream, _) = listener.accept().unwrap();
        serve(&mut g, stream).unwrap();
    });
    (port, thread)
}

#[test]
fn the_interrupt_stops_a_run_over_the_socket() {
    let (port, thread) = server(Image::AkkoRmk);
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(&frame("?")).unwrap();
    expect(&mut s, b"+$S05#b8");
    s.write_all(b"+").unwrap();
    s.write_all(&frame("QStartNoAckMode")).unwrap();
    expect(&mut s, b"$OK#9a");
    s.write_all(&frame("c")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    s.write_all(&[0x03]).unwrap();
    expect(&mut s, b"$S02#b5");
    // A bad checksum is refused, then the packet again is answered.
    s.write_all(b"$qC#00").unwrap();
    s.write_all(&frame("qC")).unwrap();
    expect(&mut s, b"$QC1#c5");
    s.write_all(&frame("D")).unwrap();
    expect(&mut s, b"$OK#9a");
    thread.join().unwrap();
}

#[test]
fn a_gdb_session() {
    let (port, thread) = server(Image::M2Blink);
    let gdb = std::env::var("GDB").unwrap_or("gdb".into());
    let out = Command::new(&gdb)
        .args(["-nx", "-batch"])
        .arg(Image::M2Blink.path())
        .args(["-ex", &format!("target remote :{port}")])
        .args(["-ex", "info registers pc xpsr msp control"])
        .args(["-ex", "break main"])
        .args(["-ex", "continue"])
        .args(["-ex", "info registers pc"])
        .args(["-ex", "stepi"])
        .args(["-ex", "x/2xw 0x08000000"])
        .args(["-ex", "set var $r1 = 0xcafe"])
        .args(["-ex", "p/x $r1"])
        .args(["-ex", "monitor time"])
        .args(["-ex", "detach"])
        .output()
        .unwrap_or_else(|e| panic!("{gdb}: {e} (set $GDB to a GDB with ARM support)"));
    thread.join().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    println!("{text}");
    // Where GDB put it (past main's prologue): the core stops there.
    let at = text
        .split("Breakpoint 1 at ")
        .nth(1)
        .and_then(|r| r.split(':').next())
        .unwrap_or_else(|| panic!("no breakpoint in {text}"))
        .to_string();
    let want = [
        "<Reset>",
        "xpsr           0x1000000",
        "control        0x0",
        "Breakpoint 1, ",
        &format!("pc             {at} "),
        "$1 = 0xcafe",
        "s emulated, ",
        "[Inferior 1 (Remote target) detached]",
    ];
    for w in want {
        assert!(text.contains(w), "no {w:?}");
    }
    let mut g = attach(Image::M2Blink);
    let vectors = format!(
        "0x8000000:\t0x{:08x}\t0x{:08x}",
        word(&mut g, 0x0800_0000),
        word(&mut g, 0x0800_0004)
    );
    assert!(text.contains(&vectors), "no {vectors:?}");
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
