//! lib8tion as QMK builds it for ARM: the C versions, `FASTLED_SCALE8_FIXED`
//! off. Each function follows the C's integer promotions.

pub fn scale8(i: u8, scale: u8) -> u8 {
    ((i as u16 * scale as u16) >> 8) as u8
}

pub fn scale16by8(i: u16, scale: u8) -> u16 {
    // (i * scale) / 256 in int.
    ((i as u32 * scale as u32) / 256) as u16
}

pub fn qadd8(i: u8, j: u8) -> u8 {
    i.saturating_add(j)
}

pub fn qsub8(i: u8, j: u8) -> u8 {
    i.saturating_sub(j)
}

pub fn abs8(i: i8) -> i8 {
    // -(-128) is -128 again once stored in an int8_t.
    if i < 0 { i.wrapping_neg() } else { i }
}

const B_M16_INTERLEAVE: [u8; 8] = [0, 49, 49, 41, 90, 27, 117, 10];

pub fn sin8(theta: u8) -> u8 {
    let mut offset = theta;
    if theta & 0x40 != 0 {
        offset = 255 - offset;
    }
    offset &= 0x3F;
    let mut secoffset = offset & 0x0F;
    if theta & 0x40 != 0 {
        secoffset += 1;
    }
    let section = (offset >> 4) as usize;
    let b = B_M16_INTERLEAVE[section * 2];
    let m16 = B_M16_INTERLEAVE[section * 2 + 1];
    let mx = ((m16 as u16 * secoffset as u16) >> 4) as u8;
    let mut y = mx.wrapping_add(b) as i8;
    if theta & 0x80 != 0 {
        y = y.wrapping_neg();
    }
    y.wrapping_add(128u8 as i8) as u8
}

pub fn cos8(theta: u8) -> u8 {
    sin8(theta.wrapping_add(64))
}

pub fn sqrt16(x: u16) -> u8 {
    if x <= 1 {
        return x as u8;
    }
    let mut low: u8 = 1;
    let mut hi: u8 = if x > 7904 { 255 } else { ((x >> 5) + 8) as u8 };
    loop {
        let mid = ((low as u16 + hi as u16) >> 1) as u8;
        if (mid as u16 * mid as u16) > x {
            hi = mid.wrapping_sub(1);
        } else {
            if mid == 255 {
                return 255;
            }
            low = mid + 1;
        }
        if hi < low {
            break;
        }
    }
    low - 1
}

pub fn atan2_8(dy: i16, dx: i16) -> u8 {
    if dy == 0 {
        return if dx >= 0 { 0 } else { 128 };
    }
    let abs_y = if dy > 0 { dy as i32 } else { -(dy as i32) } as i16 as i32;
    let dx = dx as i32;
    let a: i8 = if dx >= 0 {
        (32 - (32 * (dx - abs_y) / (dx + abs_y))) as i8
    } else {
        (96 - (32 * (dx + abs_y) / (abs_y - dx))) as i8
    };
    if dy < 0 { a.wrapping_neg() as u8 } else { a as u8 }
}

/// lib8tion's 16-bit LCG (`rand16seed`, seeded 1337).
#[derive(Clone)]
pub struct Rand {
    pub seed: u16,
}

impl Default for Rand {
    fn default() -> Self {
        Rand { seed: 1337 }
    }
}

impl Rand {
    pub fn random16(&mut self) -> u16 {
        self.seed = self.seed.wrapping_mul(2053).wrapping_add(13849);
        self.seed
    }

    pub fn random8(&mut self) -> u8 {
        let s = self.random16();
        (s as u8).wrapping_add((s >> 8) as u8)
    }

    pub fn random8_max(&mut self, lim: u8) -> u8 {
        ((self.random8() as u16 * lim as u16) >> 8) as u8
    }

    pub fn random8_min_max(&mut self, min: u8, lim: u8) -> u8 {
        self.random8_max(lim.wrapping_sub(min)).wrapping_add(min)
    }
}
