//! The board simulator from the command line.
//!   akko-sim trace <image> [secs=0.5] [block-regex=.]
//! prints the image's register writes in order, one per line ("BLOCK+0xOFF =
//! 0xVALUE"), as ../../sim/trace.sh does, so the two can be diffed
//! (compare/trace-diff.sh). The SPI flash's commands follow, as "FLASH
//! ..." lines (trace.sh has them in among the writes; they're compared as
//! a stream of their own). Then any diags (unmapped accesses, LOCKED
//! writes) to stderr.
//!   akko-sim gdb <image> [port=3333]
//! boots the image halted at its reset vector and serves GDB on
//! localhost (akko_sim::gdb), one connection after another:
//! `gdb <elf> -ex 'target remote :3333'`.
//! <image> is a name from sim-rs/target/images.

use akko_sim::gdb::Gdb;
use akko_sim::{Board, Image, Part};
use std::net::TcpListener;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage =
        "usage: akko-sim trace <image> [secs] [block-regex]\n       akko-sim gdb <image> [port]";
    let Some(name) = args.get(1) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let image = *Image::ALL
        .iter()
        .find(|i| i.file_name() == name)
        .unwrap_or_else(|| panic!("no image {name}"));
    match args[0].as_str() {
        "trace" => trace(image, &args),
        "gdb" => gdb(image, &args),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}

fn gdb(image: Image, args: &[String]) {
    let port: u16 = args
        .get(2)
        .map(|s| s.parse().expect("port"))
        .unwrap_or(3333);
    let board = Board::boot(image).unwrap_or_else(|e| panic!("{e}"));
    let mut gdb = Gdb::new(board);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    eprintln!(
        "{}: halted at 0x{:08x}; gdb {} -ex 'target remote :{port}'",
        image.file_name(),
        gdb.board.pc(),
        image
            .path()
            .canonicalize()
            .unwrap_or(image.path())
            .display()
    );
    for stream in listener.incoming() {
        match stream.map(|s| akko_sim::gdb::serve(&mut gdb, s)) {
            Ok(Ok(())) => eprintln!("gdb: disconnected"),
            Ok(Err(e)) | Err(e) => eprintln!("gdb: {e}"),
        }
    }
}

fn trace(image: Image, args: &[String]) {
    let secs: f64 = args.get(2).map(|s| s.parse().expect("secs")).unwrap_or(0.5);
    let filter =
        regex::Regex::new(args.get(3).map(String::as_str).unwrap_or(".")).expect("block-regex");

    let mut board = Board::boot(image).unwrap_or_else(|e| panic!("{e}"));
    board.chip.writes = Some(Vec::new());
    board.run_for(secs);
    for w in board.chip.writes.take().unwrap() {
        let line = w.to_string();
        if filter.is_match(&line) {
            println!("{line}");
        }
    }
    for l in board.log(Part::Nor) {
        let line = format!("FLASH {l}");
        if filter.is_match(&line) {
            println!("{line}");
        }
    }
    let mut diags: Vec<String> = board
        .chip
        .take_diags()
        .iter()
        .map(|d| d.to_string())
        .collect();
    diags.extend(board.cpu.diags.iter().map(|d| d.to_string()));
    diags.dedup();
    for d in diags.iter().take(20) {
        eprintln!("{d}");
    }
    if diags.len() > 20 {
        eprintln!("... {} more", diags.len() - 20);
    }
}
