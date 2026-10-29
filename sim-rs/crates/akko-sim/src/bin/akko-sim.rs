//! The board simulator from the command line.
//!   akko-sim trace <image> [secs=0.5] [block-regex=.]
//! prints the image's register writes in order, one per line ("BLOCK+0xOFF =
//! 0xVALUE"), as ../../sim/trace.sh does, so the two can be diffed
//! (compare/trace-diff.sh). The SPI flash's commands follow, as "FLASH
//! ..." lines (trace.sh has them in among the writes; they're compared as
//! a stream of their own). Then any diags (unmapped accesses, LOCKED
//! writes) to stderr. <image> is a name from sim-rs/target/images.

use akko_sim::{Board, Image, Part};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: akko-sim trace <image> [secs] [block-regex]";
    match args.first().map(String::as_str) {
        Some("trace") => {}
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
    let name = args.get(1).expect(usage);
    let image = *Image::ALL.iter().find(|i| i.file_name() == name).unwrap_or_else(|| panic!("no image {name}"));
    let secs: f64 = args.get(2).map(|s| s.parse().expect("secs")).unwrap_or(0.5);
    let filter = regex::Regex::new(args.get(3).map(String::as_str).unwrap_or(".")).expect("block-regex");

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
    let mut diags: Vec<String> = board.chip.take_diags().iter().map(|d| d.to_string()).collect();
    diags.extend(board.cpu.diags.iter().map(|d| d.to_string()));
    diags.dedup();
    for d in diags.iter().take(20) {
        eprintln!("{d}");
    }
    if diags.len() > 20 {
        eprintln!("... {} more", diags.len() - 20);
    }
}
