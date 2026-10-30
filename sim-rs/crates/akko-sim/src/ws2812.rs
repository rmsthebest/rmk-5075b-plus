//! The board's WS2812 chain on SPIM2's MOSI (B15). A port of the decoder
//! half of Renode's sim/peripherals/spim2.py.
//!
//! QMK's ws2812_spi encoding: 4 SPI bits per WS2812 bit (0 = 1000, 1 =
//! 1110), so every byte of a frame is two bits and never 0x00; a 0x00 byte
//! is the line held low, and so is an idle line (each byte ends low). A
//! frame starts at the first non-zero byte after the line was low for the
//! WS2812 latch's 280 us (105 bytes at 3 MHz; QMK sends 4 zero bytes before
//! and 112 after), timed on the wire, and ends at the next zero byte or
//! such a gap. 12 bytes make one LED, in G, R, B order. Errors: a byte with
//! a nibble other than 1000/1110, a frame that isn't a whole number of
//! LEDs, a frame after too short a reset, or sending in the wrong mode (CR0
//! not 8-bit Motorola mode 0 with transmit on, or SCLK not 3 MHz), or B15 not on SPIM2_MO
//! (AF5, the datasheet's alternate-function table).
//!
//! The chain's supply is switched by A8 (high: on), which the board
//! watches: while it's off the LEDs take nothing, and each time it comes
//! back they are dark until the next frame (`seen_rises`). Without a watch
//! on A8 (a bare chain) the supply is always on.

use cortex_m3_emu::TICK_HZ;
use wb32_sim::gpio::{A, B, Gpio, Pin};
use wb32_sim::ssi::{SsiDevice, SsiLine};

pub const RESET_BYTES: u32 = 105;
/// The latch: the line low this long (WS2812B datasheet, > 280 us).
const LATCH_US: u64 = 280;
const SCLK_HZ: u64 = 3_000_000;
/// The supply switch.
pub const SUPPLY: Pin = (A, 8);
const DATA: Pin = (B, 15);
const AF_SPIM: u8 = 5;

pub struct Ws2812 {
    /// When the line went low after the last frame's data (0: low since
    /// reset).
    low_since: u64,
    cur: Option<Vec<u8>>,
    /// The last frame, 0xRRGGBB per LED.
    pub leds: Vec<u32>,
    pub frames: u32,
    mode_checked: bool,
    /// A8's rising edges when the LEDs last latched: a later one means
    /// they lost power since.
    pub seen_rises: u32,
    pub errors: Vec<String>,
    /// "frame <n>, <count> LEDs, first changed <i>" for each frame that
    /// changed a colour.
    pub log: Vec<String>,
}

impl Default for Ws2812 {
    fn default() -> Self {
        Ws2812 {
            low_since: 0,
            cur: None,
            leds: Vec::new(),
            frames: 0,
            mode_checked: false,
            seen_rises: 0,
            errors: Vec::new(),
            log: Vec::new(),
        }
    }
}

fn bit(nibble: u8) -> Option<u32> {
    match nibble {
        0x8 => Some(0),
        0xE => Some(1),
        _ => None,
    }
}

impl Ws2812 {
    /// A system reset: Renode starts spim2.py again, so everything but the
    /// logs goes back to its start.
    pub fn reset(&mut self) {
        let log = std::mem::take(&mut self.log);
        let errors = std::mem::take(&mut self.errors);
        *self = Ws2812 {
            log,
            errors,
            ..Ws2812::default()
        };
    }

    fn end_frame(&mut self) {
        let data = self.cur.take().unwrap_or_default();
        if data.len() % 12 != 0 {
            self.errors.push(format!(
                "leds: frame of {} bytes, not a whole number of LEDs",
                data.len()
            ));
        }
        let mut leds = Vec::with_capacity(data.len() / 12);
        for led in data.chunks_exact(12) {
            let mut c = [0u32; 3];
            for (k, ch) in led.chunks_exact(4).enumerate() {
                for &b in ch {
                    let (hi, lo) = (bit(b >> 4), bit(b & 0xF));
                    if hi.is_none() || lo.is_none() {
                        self.errors
                            .push(format!("leds: byte 0x{b:02x} is not two WS2812 bits"));
                    }
                    c[k] = c[k] << 2 | hi.unwrap_or(0) << 1 | lo.unwrap_or(0);
                }
            }
            let [g, r, b] = c;
            leds.push(r << 16 | g << 8 | b);
        }
        self.frames += 1;
        if leds != self.leds {
            let first = leds
                .iter()
                .zip(&self.leds)
                .take_while(|(a, b)| a == b)
                .count();
            self.log.push(format!(
                "frame {}, {} LEDs, first changed {first}",
                self.frames,
                leds.len()
            ));
        }
        self.leds = leds;
    }
}

impl SsiDevice for Ws2812 {
    fn shift(&mut self, b: u8, line: SsiLine, gpio: &Gpio) -> u8 {
        if let Some(supply) = gpio.watched(SUPPLY) {
            if !supply.level {
                // Unpowered: the data goes nowhere.
                self.cur = None;
                self.low_since = 0;
                return 0;
            }
            if supply.rising != self.seen_rises {
                self.seen_rises = supply.rising;
                self.leds.fill(0);
            }
        }
        if !self.mode_checked {
            self.mode_checked = true;
            // 8-bit Motorola mode 0 (CR0 bits 7:0), sending (TMOD 00 or 01).
            if line.cr0 & 0xFF != 0x07 || line.cr0 & 0x300 > 0x100 || line.sclk_hz != SCLK_HZ {
                self.errors.push(format!(
                    "leds: CR0 0x{:x} SCLK {} Hz, not 8-bit mode 0 at 3 MHz",
                    line.cr0, line.sclk_hz
                ));
            }
            if gpio.alternate(DATA) != Some(AF_SPIM) {
                self.errors.push(format!(
                    "leds: B15 on {:?}, not SPIM2_MO (AF5)",
                    gpio.alternate(DATA)
                ));
            }
        }
        let start = line.end - line.frame;
        let low = start.saturating_sub(self.low_since);
        let latched = self.low_since == 0 || low >= LATCH_US * TICK_HZ / 1_000_000;
        if b == 0 {
            if self.cur.is_some() {
                self.end_frame();
                self.low_since = start;
            }
        } else {
            if self.cur.is_some() && latched {
                self.end_frame();
            }
            if self.cur.is_none() {
                if !latched {
                    self.errors.push(format!(
                        "leds: frame after the line was low {} us (latch needs {LATCH_US})",
                        low * 1_000_000 / TICK_HZ
                    ));
                }
                self.cur = Some(Vec::new());
            }
            self.cur.as_mut().unwrap().push(b);
            self.low_since = line.end;
        }
        // Nothing drives MISO.
        0
    }

    fn configured(&mut self) {
        self.mode_checked = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(colours: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for &c in colours {
            let grb = [(c >> 8) as u8, (c >> 16) as u8, c as u8];
            for byte in grb {
                for pair in (0..4).rev() {
                    let bits = (byte >> (pair * 2)) & 3;
                    let nib = |b: u8| if b != 0 { 0xE } else { 0x8 };
                    out.push(nib(bits >> 1) << 4 | nib(bits & 1));
                }
            }
        }
        out
    }

    /// SPIM2 at 3 MHz: a byte every 8/3 us, back to back from `t`.
    struct Wire {
        t: u64,
    }

    const FRAME: u64 = 8 * TICK_HZ / SCLK_HZ;

    impl Wire {
        fn new() -> Wire {
            Wire { t: 1 }
        }
        fn line(&mut self) -> SsiLine {
            self.t += FRAME;
            SsiLine {
                cr0: 0x07,
                baudr: 32,
                sclk_hz: SCLK_HZ,
                frame: FRAME,
                end: self.t,
            }
        }
        fn idle_us(&mut self, us: u64) {
            self.t += us * TICK_HZ / 1_000_000;
        }
    }

    #[test]
    fn decodes_frames_and_catches_a_short_latch() {
        let mut wire = Wire::new();
        // B15 on AF5 (MODER 2, AFRH nibble 7).
        let mut gpio = Gpio::default();
        gpio.write(0x400, 2 << 30, &mut |_| {});
        gpio.write(0x424, 5 << 28, &mut |_| {});
        let mut w = Ws2812::default();
        for b in encode(&[0xFF0000, 0x00FF00, 0x123456])
            .into_iter()
            .chain([0; 105])
        {
            w.shift(b, wire.line(), &gpio);
        }
        assert_eq!(w.leds, [0xFF0000, 0x00FF00, 0x123456]);
        assert_eq!((w.frames, w.errors.len()), (1, 0));
        assert_eq!(w.log, ["frame 1, 3 LEDs, first changed 0"]);
        for b in encode(&[0xFF0000])
            .into_iter()
            .chain([0; 3])
            .chain(encode(&[1]))
            .chain([0])
        {
            w.shift(b, wire.line(), &gpio);
        }
        assert_eq!(
            w.errors,
            ["leds: frame after the line was low 8 us (latch needs 280)"]
        );
        // An idle line latches as well as zero bytes do: no trailing zeros,
        // 300 us idle, then the next frame.
        w.errors.clear();
        wire.idle_us(300);
        for b in encode(&[0x0000FF]) {
            w.shift(b, wire.line(), &gpio);
        }
        wire.idle_us(300);
        for b in encode(&[0x00FF00]).into_iter().chain([0]) {
            w.shift(b, wire.line(), &gpio);
        }
        assert_eq!((w.leds.clone(), w.frames), (vec![0x00FF00], 5));
        assert!(w.errors.is_empty(), "{:?}", w.errors);
        // 200 us isn't enough.
        wire.idle_us(200);
        w.shift(0x88, wire.line(), &gpio);
        assert_eq!(
            w.errors,
            ["leds: frame after the line was low 202 us (latch needs 280)"]
        );
    }

    #[test]
    fn b15_must_be_on_af5() {
        let mut w = Ws2812::default();
        w.shift(0, Wire::new().line(), &Gpio::default());
        assert_eq!(w.errors, ["leds: B15 on None, not SPIM2_MO (AF5)"]);
    }

    /// The supply (A8) off, the chain sees no data; on again, the LEDs
    /// start dark (their power-on state) until a frame.
    #[test]
    fn unpowered_the_chain_ignores_data_and_powers_up_dark() {
        let mut wire = Wire::new();
        let mut gpio = Gpio::default();
        gpio.watch(SUPPLY);
        gpio.write(0x400, 2 << 30, &mut |_| {});
        gpio.write(0x424, 5 << 28, &mut |_| {});
        // A8 an output, high.
        gpio.write(0x000, 1 << 16, &mut |_| {});
        gpio.write(0x014, 1 << 8, &mut |_| {});
        let mut w = Ws2812::default();
        let mut frame = |w: &mut Ws2812, gpio: &Gpio, c: u32| {
            for b in encode(&[c]).into_iter().chain([0; 105]) {
                w.shift(b, wire.line(), gpio);
            }
        };
        frame(&mut w, &gpio, 0x123456);
        assert_eq!((w.leds.clone(), w.frames), (vec![0x123456], 1));
        gpio.write(0x014, 0, &mut |_| {});
        frame(&mut w, &gpio, 0xFF0000);
        assert_eq!(w.frames, 1, "unpowered");
        gpio.write(0x014, 1 << 8, &mut |_| {});
        w.shift(0, Wire::new().line(), &gpio);
        assert_eq!(w.leds, [0]);
        frame(&mut w, &gpio, 0x00FF00);
        assert_eq!((w.leds.clone(), w.frames), (vec![0x00FF00], 2));
        assert_eq!(w.errors, Vec::<String>::new());
    }
}
