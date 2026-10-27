//! QMK's color.c, without the CIE1931 curve (this board doesn't enable it).

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Hsv {
    pub h: u8,
    pub s: u8,
    pub v: u8,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const OFF: Rgb = Rgb { r: 0, g: 0, b: 0 };
}

/// `hsv_to_rgb_impl(hsv, false)`.
pub fn hsv_to_rgb(hsv: Hsv) -> Rgb {
    let v8 = hsv.v;
    if hsv.s == 0 {
        return Rgb {
            r: v8,
            g: v8,
            b: v8,
        };
    }
    let (h, s, v) = (hsv.h as i32, hsv.s as i32, hsv.v as i32);
    let region = (h * 6 / 255) as u8;
    let remainder = ((h * 2 - region as i32 * 85) * 3) as u8 as i32;
    let p = ((v * (255 - s)) >> 8) as u8;
    let q = ((v * (255 - ((s * remainder) >> 8))) >> 8) as u8;
    let t = ((v * (255 - ((s * (255 - remainder)) >> 8))) >> 8) as u8;
    let (r, g, b) = match region {
        6 | 0 => (v8, t, p),
        1 => (q, v8, p),
        2 => (p, v8, t),
        3 => (p, q, v8),
        4 => (t, p, v8),
        _ => (v8, p, q),
    };
    Rgb { r, g, b }
}
