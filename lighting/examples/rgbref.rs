//! The Rust side of tools/rgbref: runs the harness script (see
//! tools/rgbref/harness.c) on `Engine` and prints frames the same way.

use std::io::{BufRead, Write};

use lighting::{Config, Engine, Hsv, NoHooks};

fn main() {
    let mut e = Engine::new(Config { enable: false, mode: 0, hsv: Hsv::default(), speed: 0, flags: 0 });
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let mut print = |now: u32, e: &Engine| {
        write!(out, "{now} ").unwrap();
        for c in &e.leds.rgb {
            write!(out, "{:02x}{:02x}{:02x}", c.r, c.g, c.b).unwrap();
        }
        writeln!(out).unwrap();
    };
    let mut now = 0;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut w = line.split_whitespace();
        let cmd = w.next().unwrap_or("");
        let a: Vec<u32> = w.map(|x| x.parse().unwrap()).collect();
        match cmd {
            "cfg" => {
                // The C config packs enable into 2 bits and mode into 6.
                e.config = Config {
                    enable: a[0] & 3 != 0,
                    mode: (a[1] & 63) as u8,
                    hsv: Hsv { h: a[2] as u8, s: a[3] as u8, v: a[4] as u8 },
                    speed: a[5] as u8,
                    flags: a[6] as u8,
                }
            }
            "key" => e.key_event(a[0] as u8, a[1] as u8, a[2] != 0),
            "run" => {
                let mut t = a[0];
                while t < a[1] {
                    now = t;
                    if e.task(t, &mut NoHooks) {
                        print(t, &e);
                    }
                    t += a[2];
                }
            }
            "suspend" => {
                if e.set_suspended(a[0] != 0, &mut NoHooks) {
                    print(now, &e);
                }
            }
            "" => {}
            _ if cmd.starts_with('#') => {}
            _ => panic!("bad line: {line}"),
        }
    }
}
