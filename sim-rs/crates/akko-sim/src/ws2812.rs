//! The board's WS2812 chain on SPIM2's MOSI (B15). A port of the decoder
//! half of ../../sim/peripherals/spim2.py.
//!
//! QMK's ws2812_spi encoding: 4 SPI bits per WS2812 bit (0 = 1000, 1 =
//! 1110), so every byte of a frame is two bits and never 0x00; a 0x00 byte
//! is the line held low. A frame starts at the first non-zero byte after at
//! least RESET_BYTES zero bytes (QMK sends 4 before and 112 after; the
//! WS2812 latch needs 280 us = 105 bytes at 3 MHz) and ends at the next
//! zero byte. 12 bytes make one LED, in G, R, B order. Errors: a byte with
//! a nibble other than 1000/1110, a frame that isn't a whole number of
//! LEDs, a frame after too short a reset, or sending in the wrong mode (CR0
//! not 8-bit Motorola mode 0, or BAUDR not 32 = 3 MHz).

use wb32_sim::gpio::Gpio;
use wb32_sim::ssi::{SsiDevice, SsiLine};

pub const RESET_BYTES: u32 = 105;
const BAUDR: u32 = 32;

pub struct Ws2812 {
    /// Zero bytes since the last frame (the line was low since reset).
    zeros: u32,
    cur: Option<Vec<u8>>,
    /// The last frame, 0xRRGGBB per LED.
    pub leds: Vec<u32>,
    pub frames: u32,
    mode_checked: bool,
    pub errors: Vec<String>,
    /// "frame <n>, <count> LEDs, first changed <i>" for each frame that
    /// changed a colour.
    pub log: Vec<String>,
}

impl Default for Ws2812 {
    fn default() -> Self {
        Ws2812 {
            zeros: RESET_BYTES,
            cur: None,
            leds: Vec::new(),
            frames: 0,
            mode_checked: false,
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
    fn shift(&mut self, b: u8, line: SsiLine, _gpio: &Gpio) -> u8 {
        if !self.mode_checked {
            self.mode_checked = true;
            if line.cr0 & 0x3FF != 0x07 || line.baudr != BAUDR {
                self.errors.push(format!(
                    "leds: CR0 0x{:x} BAUDR {}, not 8-bit mode 0 at 3 MHz",
                    line.cr0, line.baudr
                ));
            }
        }
        if b == 0 {
            if self.cur.is_some() {
                self.end_frame();
            }
            self.zeros += 1;
        } else {
            if self.cur.is_none() {
                if self.zeros < RESET_BYTES {
                    self.errors.push(format!(
                        "leds: frame after only {} zero bytes (latch needs {RESET_BYTES})",
                        self.zeros
                    ));
                }
                self.cur = Some(Vec::new());
            }
            self.zeros = 0;
            self.cur.as_mut().unwrap().push(b);
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

    #[test]
    fn decodes_frames_and_catches_a_short_latch() {
        let line = SsiLine {
            cr0: 0x07,
            baudr: 32,
        };
        let gpio = Gpio::default();
        let mut w = Ws2812::default();
        for b in encode(&[0xFF0000, 0x00FF00, 0x123456])
            .into_iter()
            .chain([0; 105])
        {
            w.shift(b, line, &gpio);
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
            w.shift(b, line, &gpio);
        }
        assert_eq!(
            w.errors,
            ["leds: frame after only 3 zero bytes (latch needs 105)"]
        );
    }
}
