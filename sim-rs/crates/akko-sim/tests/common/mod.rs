//! What the board suites (m4, m5, m6) share: the robot files' keywords.
//!
//! Renode's log tester is one log: a wait consumes the line it matched and
//! everything before it, from any peripheral. Here the host, the flash and
//! the radio log separately, each with a cursor; a match on one moves the
//! others' to their end, which comes to the same thing within a host step.
#![allow(dead_code)]

use akko_sim::usb_host::{UsbEvent, cmd, hex};
use akko_sim::{Board, Image, Part};
use regex::Regex;
use std::sync::OnceLock;
use wb32_sim::usb::UsbAccess;

pub const GPIOA_ODR: u32 = 0x4000_0014;
pub const GPIOB_ODR: u32 = 0x4000_0414;
pub const GPIOC_MODER: u32 = 0x4000_0800;
pub const GPIOC_ODR: u32 = 0x4000_0814;
pub const CAPS_LED: u32 = 0x20;
pub const WIN_LOCK_LED: u32 = 0x01;
pub const AUX_LED: u32 = 0x200;
pub const LED_POWER: u32 = 0x100;
pub const LED_BOOST: u32 = 0x200;
pub const USB_POWER: u32 = 0x02;
pub const VIAL_OUT_EP: u32 = 2;
pub const SYST_CSR: u32 = 0xE000_E010;
pub const SCB_SCR: u32 = 0xE000_ED10;

pub struct T {
    pub b: Board,
    pub image: Image,
    pub usb: usize,
    pub nor: usize,
    pub radio: usize,
    pub trace: usize,
    /// Errors the test means to cause (`allow_errors`).
    pub errors_allowed: bool,
}

/// A test ends with no errors (`Board::errors`) unless it calls
/// `allow_errors`; one that has already failed isn't checked again.
impl Drop for T {
    fn drop(&mut self) {
        if !self.errors_allowed && !std::thread::panicking() {
            self.no_errors();
        }
    }
}

impl T {
    /// Loaded and reset, from a blank flash; not run yet.
    pub fn start(image: Image) -> T {
        T {
            b: Board::boot(image).unwrap(),
            image,
            usb: 0,
            nor: 0,
            radio: 0,
            trace: 0,
            errors_allowed: false,
        }
    }

    /// The test causes errors on purpose (and checks them itself).
    pub fn allow_errors(&mut self) {
        self.errors_allowed = true;
    }

    pub fn boot() -> T {
        T::start(Image::AkkoRmk)
    }

    pub fn boot_and_enumerate() -> T {
        let mut t = T::boot();
        t.wait_for_enumeration();
        t
    }

    /// Every log's cursor to its end but `except`'s.
    fn consumed(&mut self, except: Option<Part>) {
        if except.is_some() {
            self.usb = self.b.host.events().len();
        }
        if except != Some(Part::Nor) {
            self.nor = self.b.log(Part::Nor).len();
        }
        if except != Some(Part::Radio) {
            self.radio = self.b.log(Part::Radio).len();
        }
    }

    pub fn wait(&mut self, secs: f64, what: &str, pred: impl FnMut(&UsbEvent) -> bool) -> UsbEvent {
        match self.b.wait_for(&mut self.usb, secs, pred) {
            Some(e) => {
                self.consumed(None);
                e
            }
            None => {
                let tail: Vec<String> = self
                    .b
                    .host
                    .events()
                    .iter()
                    .rev()
                    .take(12)
                    .map(|e| e.to_string())
                    .collect();
                panic!("no {what} within {secs} s; last events, newest first: {tail:#?}")
            }
        }
    }

    /// A host log line containing `line` (Renode matches substrings).
    pub fn log_should_show(&mut self, line: &str, secs: f64) {
        self.wait(
            secs,
            line,
            |e| matches!(e, UsbEvent::Log(l) if l.contains(line)),
        );
    }

    /// A host log line matching `pattern`; returns it.
    pub fn log_matching(&mut self, pattern: &str, secs: f64) -> String {
        let re = Regex::new(pattern).unwrap();
        match self.wait(
            secs,
            pattern,
            |e| matches!(e, UsbEvent::Log(l) if re.is_match(l)),
        ) {
            UsbEvent::Log(l) => l,
            _ => unreachable!(),
        }
    }

    /// Nothing in the host's log since the last wait contains `text`
    /// (in any event's text, as "HOST ERROR: ...").
    pub fn host_not_since(&self, text: &str) {
        let events = self.b.host.events();
        let hit = events[self.usb..]
            .iter()
            .map(|e| e.to_string())
            .find(|l| l.contains(text));
        assert!(hit.is_none(), "{hit:?}");
    }

    pub fn wait_for_enumeration(&mut self) {
        self.log_should_show("configured", 4.0);
        // Let the start-up switch tap and its release go through.
        self.b.run_for(0.05);
    }

    /// A report on `ep` starting with `want` (Renode matches its log
    /// lines as substrings).
    pub fn hid_should_show(&mut self, ep: u8, want: &str, secs: f64) {
        self.wait(secs, &format!("HID {ep}: {want}"), |e| {
            matches!(e, UsbEvent::Hid { ep: e_ep, data } if *e_ep == ep && hex(data).starts_with(want))
        });
    }

    /// The next report on `ep` whose hex matches `pattern` from its start.
    pub fn hid_matching(&mut self, ep: u8, pattern: &str, secs: f64) -> Vec<u8> {
        let re = Regex::new(&format!("^{pattern}")).unwrap();
        match self.wait(secs, &format!("HID {ep}: {pattern}"), |e| {
            matches!(e, UsbEvent::Hid { ep: e_ep, data } if *e_ep == ep && re.is_match(&hex(data)))
        }) {
            UsbEvent::Hid { data, .. } => data,
            _ => unreachable!(),
        }
    }

    /// Nothing on `ep` starting with `pattern` since the last wait.
    pub fn hid_not_since(&self, ep: u8, pattern: &str) {
        let re = Regex::new(&format!("^{pattern}")).unwrap();
        let events = self.b.host.events();
        let hit = events[self.usb..]
            .iter()
            .find(|e| matches!(e, UsbEvent::Hid { ep: e_ep, data } if *e_ep == ep && re.is_match(&hex(data))));
        assert!(hit.is_none(), "{hit:?}");
    }

    /// The next line of a part's log matching `pattern`.
    pub fn part_should_show(&mut self, part: Part, pattern: &str, secs: f64) -> String {
        let re = Regex::new(pattern).unwrap();
        let mut cursor = match part {
            Part::Nor => self.nor,
            Part::Radio => self.radio,
            Part::Leds => 0,
        };
        let Some(line) = self.b.wait_log(part, &mut cursor, secs, |l| re.is_match(l)) else {
            let log = self.b.log(part);
            let tail: Vec<&String> = log.iter().rev().take(12).collect();
            panic!("no {part:?} {pattern} within {secs} s; last lines, newest first: {tail:#?}");
        };
        match part {
            Part::Nor => self.nor = cursor,
            Part::Radio => self.radio = cursor,
            Part::Leds => {}
        }
        self.consumed(Some(part));
        line
    }

    /// Nothing in a part's log since the last wait matches `pattern`.
    pub fn part_not_since(&self, part: Part, pattern: &str) {
        let re = Regex::new(pattern).unwrap();
        let from = match part {
            Part::Nor => self.nor,
            Part::Radio => self.radio,
            Part::Leds => 0,
        };
        let log = self.b.log(part);
        let hit = log[from..].iter().find(|l| re.is_match(l));
        assert!(hit.is_none(), "{part:?}: {hit:?}");
    }

    pub fn key(&mut self, row: u8, col: u8, down: bool) {
        self.b.key(row, col, down);
    }

    pub fn tap(&mut self, row: u8, col: u8) {
        self.key(row, col, true);
        self.b.run_for(0.03);
        self.key(row, col, false);
        self.b.run_for(0.03);
    }

    /// Fn held, (row, col) tapped `times` times, Fn released.
    pub fn fn_tap(&mut self, (row, col): (u8, u8), times: u32) {
        self.key(5, 10, true);
        self.b.run_for(0.03);
        for _ in 0..times {
            self.tap(row, col);
        }
        self.key(5, 10, false);
        self.b.run_for(0.03);
    }

    /// Fn tap, then past the black blink that confirms it; `times` times.
    pub fn fn_tap_and_wait(&mut self, pos: (u8, u8), times: u32) {
        for _ in 0..times {
            self.fn_tap(pos, 1);
            self.b.run_for(0.6);
        }
    }

    /// Fn held, `pos` held `secs`, both released.
    pub fn fn_hold(&mut self, (row, col): (u8, u8), secs: f64) {
        self.key(5, 10, true);
        self.b.run_for(0.03);
        self.key(row, col, true);
        self.b.run_for(secs);
        self.key(row, col, false);
        self.b.run_for(0.03);
        self.key(5, 10, false);
        self.b.run_for(0.03);
    }

    /// One detent of the knob (parts::Knob), `edge` seconds between
    /// quadrature edges. Clockwise: A leads, as QMK decodes this knob.
    pub fn knob_step(&mut self, clockwise: bool, edge: f64) {
        self.knob_step_bouncing(clockwise, edge, 0, 0.0);
    }

    /// As `knob_step`, each edge's contact bouncing `bounces` times
    /// (open and closed again) `bounce` seconds apart before it settles.
    pub fn knob_step_bouncing(&mut self, clockwise: bool, edge: f64, bounces: u32, bounce: f64) {
        for _ in 0..4 {
            self.b.knob_edge(clockwise);
            for _ in 0..bounces {
                self.b.run_for(bounce);
                self.b.knob_edge(!clockwise);
                self.b.run_for(bounce);
                self.b.knob_edge(clockwise);
            }
            self.b.run_for(edge);
        }
    }

    /// The Win/Mac switch to Mac (true) or Win.
    pub fn mac_mode(&mut self, on: bool) {
        self.b.mac_switch(on);
        self.b.run_for(0.05);
    }

    /// One 32-byte packet on the Vial OUT endpoint, `bytes` then zeros.
    pub fn raw_hid_send(&mut self, bytes: &[u8]) {
        let mut p = bytes.to_vec();
        p.resize(32, 0);
        for b in p {
            self.b.host.out_byte(b);
        }
        self.b.host.command(cmd::OUT_PACKET | VIAL_OUT_EP << 8);
    }

    /// raw_hid_send, then 50 ms (m5's and m6's Raw HID Send).
    pub fn raw_hid_send_and_run(&mut self, bytes: &[u8]) {
        self.raw_hid_send(bytes);
        self.b.run_for(0.05);
    }

    /// Vial channel 0xD2 (the previous run's panic message) is empty.
    pub fn no_panic_since_start_up(&mut self) {
        self.raw_hid_send(&[0x08, 0xD2, 0x00]);
        self.hid_should_show(3, "08 d2 00 00", 1.0);
    }

    pub fn pin_high(&mut self, odr: u32, mask: u32) -> bool {
        self.b.word(odr) & mask != 0
    }

    pub fn led_power(&mut self) -> bool {
        self.pin_high(GPIOA_ODR, LED_POWER)
    }

    pub fn win_lock_led_on(&mut self) -> bool {
        self.b.word(GPIOB_ODR) & WIN_LOCK_LED == 0
    }

    /// The next USB register write since the last one found, as usb.py's
    /// "USBT W" lines.
    pub fn usb_write_should_be(&mut self, want: &str, secs: f64) {
        let mut seen = self.trace;
        let found = self.b.run_until(secs, |b| {
            let d = b.chip.usb.borrow();
            let trace = d.trace.as_ref().unwrap();
            match trace[seen..]
                .iter()
                .position(|a: &UsbAccess| a.write && a.to_string() == want)
            {
                Some(i) => {
                    seen += i + 1;
                    true
                }
                None => {
                    seen = trace.len();
                    false
                }
            }
        });
        assert!(found, "no {want} within {secs} s");
        self.trace = seen;
    }

    pub fn pc_should_be_in_rom(&self) {
        assert!(self.b.in_rom(), "PC 0x{:08x}", self.b.pc());
    }

    /// A system reset (AIRCR SYSRESETREQ), the flash kept; on until USB is
    /// configured.
    pub fn sys_reset_and_enumerate(&mut self, secs: f64) {
        self.b.system_reset();
        self.log_should_show("configured", secs);
        self.b.run_for(0.05);
    }

    pub fn stack_start(&self) -> u32 {
        self.image.symbol("_stack_start").expect("_stack_start")
    }

    /// The colours of the last frame: all 106 LEDs.
    pub fn led_colours(&self) -> Vec<u32> {
        let c = self.b.led_colours();
        assert_eq!(c.len(), 106);
        c
    }

    pub fn led(&self, i: usize) -> u32 {
        self.b.led_colours().get(i).copied().unwrap_or(0xFFFF_FFFF)
    }

    pub fn led_frames(&self) -> u32 {
        self.b.leds.borrow().frames
    }

    /// The battery's level (the radio module reports it), then past the
    /// module's next inquiry (every 3 s) and a frame.
    pub fn set_battery(&mut self, percent: u32) {
        self.b.charger.level = percent as f64;
        self.b.run_for(3.1);
    }

    pub fn no_led_errors(&self) {
        let leds = self.b.leds.borrow();
        let mut e = leds.errors.clone();
        e.extend(self.b.chip.spim2.errors.iter().cloned());
        assert_eq!(e, Vec::<String>::new());
    }

    pub fn no_errors(&self) {
        assert_eq!(self.b.errors(), Vec::<String>::new());
    }
}

/// QMK's hsv_to_rgb (no CIE curve), as 0xRRGGBB.
pub fn hsv_to_rgb(h: u32, s: u32, v: u32) -> u32 {
    if s == 0 {
        return v << 16 | v << 8 | v;
    }
    let region = h * 6 / 255;
    let rem = ((h * 2 - region * 85) * 3) & 0xFF;
    let p = (v * (255 - s)) >> 8;
    let q = (v * (255 - ((s * rem) >> 8))) >> 8;
    let t = (v * (255 - ((s * (255 - rem)) >> 8))) >> 8;
    let (r, g, b) = match region {
        0 | 6 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    r << 16 | g << 8 | b
}

/// The LED layout from crates/akko-lighting/src/layout.rs: board data, the
/// one thing the tests take from the firmware's sources (as leds.py).
pub struct Layout {
    pub points: Vec<(u32, u32)>,
    pub matrix_co: Vec<Vec<Option<usize>>>,
}

/// The side lights (rgblight) are the last 22 LEDs; the rest are keys.
pub const SIDE: std::ops::Range<usize> = 84..106;

pub fn layout() -> &'static Layout {
    static L: OnceLock<Layout> = OnceLock::new();
    L.get_or_init(|| {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../crates/akko-lighting/src/layout.rs"
        );
        let src = std::fs::read_to_string(path).unwrap();
        let section = |name: &str| {
            let after = src.split(name).nth(1).unwrap();
            let after = after.split_once('=').unwrap().1;
            after.split("];").next().unwrap().to_string()
        };
        let pt = Regex::new(r"\((\d+), (\d+)\)").unwrap();
        let points = pt
            .captures_iter(&section("pub const POINTS"))
            .map(|c| (c[1].parse().unwrap(), c[2].parse().unwrap()))
            .collect();
        let row = Regex::new(r"\[([^\[\]]*)\]").unwrap();
        let val = Regex::new(r"NO_LED|\d+").unwrap();
        let matrix_co = row
            .captures_iter(&section("pub const MATRIX_CO:"))
            .map(|r| {
                val.find_iter(&r[1])
                    .map(|v| v.as_str().parse().ok())
                    .collect()
            })
            .collect();
        Layout { points, matrix_co }
    })
}

/// The effect time t for which every key LED shows CYCLE_LEFT_RIGHT's
/// colour, hue = x - t, or fail. (The side lights show their own effect;
/// LED 0 is the battery's, dark unless the battery is low or charging.)
pub fn cycle_left_right_time(colours: &[u32], s: u32, v: u32) -> u32 {
    let points = &layout().points;
    (0..256)
        .find(|t| {
            colours[..SIDE.start]
                .iter()
                .zip(points)
                .skip(1)
                .all(|(&c, &(x, _))| c == hsv_to_rgb((x.wrapping_sub(*t)) & 0xFF, s, v))
        })
        .unwrap_or_else(|| {
            panic!(
                "no time fits cycle_left_right: {}",
                colours[..SIDE.start]
                    .iter()
                    .map(|c| format!("{c:06x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
}

/// At QMK's defaults: saturation 255, brightness 84.
pub fn cycle_time(colours: &[u32]) -> u32 {
    cycle_left_right_time(colours, 255, 84)
}

/// The hue h for which the side lights show the vendor's rainbow swirl,
/// light i (and 21 - i) at hue 11 i + h, or fail.
pub fn swirl_hue(colours: &[u32]) -> u32 {
    let side = &colours[SIDE];
    (0..256)
        .find(|h| {
            (0..11).all(|i| {
                side[i] == side[21 - i]
                    && side[i] == hsv_to_rgb((11 * i as u32 + h) & 0xFF, 255, 120)
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "the side lights aren't a swirl: {}",
                side.iter()
                    .map(|c| format!("{c:06x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
}

/// The side lights' colour, if they're all one colour, else fail.
pub fn side_colour(colours: &[u32]) -> u32 {
    let side = &colours[SIDE];
    assert!(
        side.iter().all(|&c| c == side[0]),
        "the side lights differ: {side:06x?}"
    );
    side[0]
}

/// Indices of the LEDs that aren't off.
pub fn lit(colours: &[u32]) -> Vec<usize> {
    colours
        .iter()
        .enumerate()
        .filter(|(_, c)| **c != 0)
        .map(|(i, _)| i)
        .collect()
}

/// Indices of the LEDs under keys that aren't off. (LED 0 has no key.)
pub fn lit_keys(colours: &[u32]) -> Vec<usize> {
    let keys: Vec<usize> = layout()
        .matrix_co
        .iter()
        .flatten()
        .flatten()
        .copied()
        .filter(|&i| i < SIDE.start)
        .collect();
    lit(colours)
        .into_iter()
        .filter(|i| keys.contains(i))
        .collect()
}

/// A golden trace from ../../golden/qmk (recorded in Renode), its RCC
/// writes as they come from the RM's reset values. Renode's RCC resets
/// every register to 0; the RM's RCC_AHBENR1 is 0x3D (7.3.12), and
/// firmware sets its bits with a read-modify-write, which keeps those.
pub fn golden(name: &str) -> String {
    let path = format!("{}/../../golden/qmk/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .map(|l| match l.strip_prefix("RCC+0x040 = 0x") {
            Some(v) => {
                let v = u32::from_str_radix(v, 16).unwrap() | 0x3D;
                format!("RCC+0x040 = 0x{v:08x}\n")
            }
            None => format!("{l}\n"),
        })
        .collect()
}

/// A golden trace's lines with `block`'s SSI FIFO `depth` deep instead of
/// Renode's 8: ChibiOS's probe (and our HAL's copy) writes TXFTLR 1, 2, ...
/// until one is refused, so up to `depth`, and then sets the thresholds
/// TXFTLR = depth / 2 and RXFTLR = depth / 2 - 1.
pub fn at_fifo_depth(golden: &[&str], block: &str, depth: u32) -> Vec<String> {
    let txftlr = format!("{block}+0x018 = 0x");
    let rxftlr = format!("{block}+0x01c = 0x");
    let value = |l: &str, p: &str| {
        l.strip_prefix(p)
            .map(|v| u32::from_str_radix(v, 16).unwrap())
    };
    let mut out = Vec::new();
    let mut probing = true;
    for &l in golden {
        match (value(l, &txftlr), value(l, &rxftlr)) {
            (Some(n), _) if probing && n >= 1 => {
                if n <= depth {
                    out.push(l.to_string());
                }
                probing = n < 8;
            }
            (Some(4), _) => out.push(format!("{txftlr}{:08x}", depth / 2)),
            (_, Some(3)) => out.push(format!("{rxftlr}{:08x}", depth / 2 - 1)),
            _ => out.push(l.to_string()),
        }
    }
    out
}

/// Consecutive repeats folded into one: a write the RM's reset values add
/// next to the same write (an interrupt handler that runs once more).
pub fn fold_repeats<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().map(|l| l.as_ref().to_string()).collect();
    out.dedup();
    out
}
