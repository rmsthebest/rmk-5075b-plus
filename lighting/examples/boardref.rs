//! The Rust side of tools/rgbref/board.py: runs the board harness script
//! (see tools/rgbref/board_harness.c) on `Board` and prints the same way.
//! The first line carries the default recordings: `defaults <360 numbers>`.
//! The lights-off battery flash is off: the vendor's C has none.

use std::io::{BufRead, Write};

use lighting::board::{CHANNELS, KEYS};
use lighting::{Board, Key};

fn key(name: &str) -> Key {
    match name {
        "RGB_TOG" => Key::RgbTog,
        "RGB_MOD" => Key::RgbMod,
        "RGB_RMOD" => Key::RgbRmod,
        "RGB_HUI" => Key::RgbHui,
        "RGB_HUD" => Key::RgbHud,
        "RGB_SAI" => Key::RgbSai,
        "RGB_SAD" => Key::RgbSad,
        "RGB_VAI" => Key::RgbVai,
        "RGB_VAD" => Key::RgbVad,
        "RGB_SPI" => Key::RgbSpi,
        "RGB_SPD" => Key::RgbSpd,
        "RL_MOD" => Key::RlMod,
        "RP_P0" => Key::RpPlay(0),
        "RP_P1" => Key::RpPlay(1),
        "RP_P2" => Key::RpPlay(2),
        "RP_END" => Key::RpEnd,
        "NK_TOGG" => Key::NkTogg,
        "EE_CLR" => Key::EeClr,
        "KC_ENCW" => Key::Encw,
        "HS_BATQ" => Key::BatQ,
        "MO_FL" => Key::Fn,
        "MO_FBL" => Key::Momentary,
        "KC_A" => Key::Other,
        _ => panic!("bad key: {name}"),
    }
}

fn main() {
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let mut lines = std::io::stdin().lock().lines();
    let first = lines.next().unwrap().unwrap();
    let nums: Vec<u16> = first
        .split_whitespace()
        .skip(1)
        .map(|x| x.parse().unwrap())
        .collect();
    let mut defaults = [[0u16; KEYS]; CHANNELS];
    for (i, n) in nums.iter().enumerate() {
        defaults[i / KEYS][i % KEYS] = *n;
    }
    let defaults: &'static _ = Box::leak(Box::new(defaults));

    let mut b: Option<Board> = None;
    let mut usb = true;
    let mut bat = (100, false, false);
    let mut now = 0;
    let mut power: Option<bool> = None;
    macro_rules! power_check {
        ($b:expr) => {
            if power != Some($b.vendor.power) {
                power = Some($b.vendor.power);
                writeln!(out, "P {now} {}", $b.vendor.power as u8).unwrap();
            }
        };
    }
    for line in lines {
        let line = line.unwrap();
        let w: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| -> u32 { w[i].parse().unwrap() };
        match w.first().copied().unwrap_or("") {
            "boot" => {
                let mut nb = Board::new(None, defaults, n(1) != 0 && n(2) == 0, now);
                nb.vendor.usb_active = usb;
                nb.vendor.bat_flash = false;
                (nb.vendor.bat, nb.vendor.charging, nb.vendor.full) = bat;
                power_check!(nb);
                b = Some(nb);
            }
            "key" => {
                let b = b.as_mut().unwrap();
                let (row, col, pressed) = (n(2) as u8, n(3) as u8, n(4) != 0);
                b.engine.key_event(row, col, pressed);
                b.key(key(w[1]), row, col, pressed, n(5) != 0, now);
                power_check!(b);
            }
            "run" => {
                let b = b.as_mut().unwrap();
                let mut t = n(1);
                while t < n(2) {
                    now = t;
                    if b.task(t) {
                        write!(out, "{t} ").unwrap();
                        for c in &b.engine.leds.rgb {
                            write!(out, "{:02x}{:02x}{:02x}", c.r, c.g, c.b).unwrap();
                        }
                        writeln!(out).unwrap();
                    }
                    power_check!(b);
                    if b.vendor.shutdown_requested {
                        b.vendor.shutdown_requested = false;
                        writeln!(out, "S {t}").unwrap();
                    }
                    t += n(3);
                }
            }
            "devs" => b.as_mut().unwrap().devs_change(n(1) as u8, n(2) != 0, now),
            "wls_key" => b.as_mut().unwrap().wls_key(n(1) as u8, now),
            "wls_pair" => b.as_mut().unwrap().wls_pair(n(1) as u8, now),
            "state" => b.as_mut().unwrap().vendor.md_state = n(1) as u8,
            "bat" => {
                bat = (n(1) as u8, n(2) != 0, n(3) != 0);
                if let Some(b) = b.as_mut() {
                    (b.vendor.bat, b.vendor.charging, b.vendor.full) = bat;
                }
            }
            "usb" => {
                usb = n(1) != 0;
                if let Some(b) = b.as_mut() {
                    b.vendor.usb_active = usb;
                }
            }
            "dump" => {
                let b = b.as_ref().unwrap();
                let (m, l, s) = (b.engine.config, b.vendor.rgblight.config, b.saved());
                write!(
                    out,
                    "D m {} {} {} {} {} {} {}",
                    m.enable as u8, m.mode, m.hsv.h, m.hsv.s, m.hsv.v, m.speed, m.flags
                )
                .unwrap();
                write!(
                    out,
                    " l {} {} {} {} {} {}",
                    l.enable as u8, l.mode, l.hsv.h, l.hsv.s, l.hsv.v, l.speed
                )
                .unwrap();
                write!(
                    out,
                    " c {} {} {} {}",
                    s.conf.record_channel,
                    s.conf.record_last_mode,
                    s.conf.rgb_enable as u8,
                    b.vendor.led_status as u8
                )
                .unwrap();
                write!(out, " i").unwrap();
                for c in s.colours {
                    write!(out, " {c}").unwrap();
                }
                write!(out, " r").unwrap();
                for c in s.channels.iter().flatten() {
                    write!(out, " {c}").unwrap();
                }
                writeln!(out).unwrap();
            }
            "" => {}
            c if c.starts_with('#') => {}
            _ => panic!("bad line: {line}"),
        }
    }
}
