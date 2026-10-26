use std::io::Write;
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy("memory.x", out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");

    // The Vial definition (tools/via2vial.py writes vial.json): minified and
    // xz compressed, as QMK-Vial serves it.
    println!("cargo:rerun-if-changed=vial.json");
    let json: serde_json::Value = serde_json::from_slice(&fs::read("vial.json").unwrap()).unwrap();
    let min = serde_json::to_vec(&json).unwrap();
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut &min[..], &mut xz).unwrap();
    let mut f = fs::File::create(out.join("vial_generated.rs")).unwrap();
    writeln!(f, "/// Identifies this board to Vial (random, fixed).").unwrap();
    writeln!(f, "pub const VIAL_KEYBOARD_ID: [u8; 8] = {:?};", VIAL_KEYBOARD_ID).unwrap();
    writeln!(f, "/// vial.json, minified and xz compressed ({} -> {} bytes).", min.len(), xz.len()).unwrap();
    writeln!(f, "pub const VIAL_KEYBOARD_DEF: [u8; {}] = {:?};", xz.len(), xz).unwrap();
}

const VIAL_KEYBOARD_ID: [u8; 8] = [0x8a, 0x5c, 0x3e, 0x91, 0x07, 0xd2, 0x6b, 0xf4];
