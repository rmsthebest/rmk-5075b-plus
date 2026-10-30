//! VialRGB, Vial's lighting protocol (vial-qmk quantum/vialrgb.c): its
//! effect ids and the commands, over VIA's custom set/get value (0x07,
//! 0x08) with the command in byte 1 and its arguments from byte 2.
//!
//! Effects Vial has no id for (pixel flow, the board's RGBR_PLAY) read as
//! Off, as vial-qmk reports a mode missing from its table. Then a slider
//! sends Off back with its value: `set_mode` keeps the effect (vial-qmk
//! would turn the lighting off).

use crate::effect;
use crate::engine::Engine;

pub const PROTOCOL_VERSION: u16 = 1;
pub const GET_INFO: u8 = 0x40;
/// Get, and set (with 0x07).
pub const MODE: u8 = 0x41;
pub const GET_SUPPORTED: u8 = 0x42;

pub const OFF: u16 = 0;

/// Ours for each VialRGB id (vialrgb_effects.inc's enum order), or None:
/// 0 Off and 1 Direct have no effect of ours.
const EFFECTS: [Option<u8>; 45] = {
    use effect::*;
    [
        None,
        None,
        Some(SOLID_COLOR),
        Some(ALPHAS_MODS),
        Some(GRADIENT_UP_DOWN),
        Some(GRADIENT_LEFT_RIGHT),
        Some(BREATHING),
        Some(BAND_SAT),
        Some(BAND_VAL),
        Some(BAND_PINWHEEL_SAT),
        Some(BAND_PINWHEEL_VAL),
        Some(BAND_SPIRAL_SAT),
        Some(BAND_SPIRAL_VAL),
        Some(CYCLE_ALL),
        Some(CYCLE_LEFT_RIGHT),
        Some(CYCLE_UP_DOWN),
        Some(RAINBOW_MOVING_CHEVRON),
        Some(CYCLE_OUT_IN),
        Some(CYCLE_OUT_IN_DUAL),
        Some(CYCLE_PINWHEEL),
        Some(CYCLE_SPIRAL),
        Some(DUAL_BEACON),
        Some(RAINBOW_BEACON),
        Some(RAINBOW_PINWHEELS),
        Some(RAINDROPS),
        Some(JELLYBEAN_RAINDROPS),
        Some(HUE_BREATHING),
        Some(HUE_PENDULUM),
        Some(HUE_WAVE),
        Some(TYPING_HEATMAP),
        Some(DIGITAL_RAIN),
        Some(SOLID_REACTIVE_SIMPLE),
        Some(SOLID_REACTIVE),
        Some(SOLID_REACTIVE_WIDE),
        Some(SOLID_REACTIVE_MULTIWIDE),
        Some(SOLID_REACTIVE_CROSS),
        Some(SOLID_REACTIVE_MULTICROSS),
        Some(SOLID_REACTIVE_NEXUS),
        Some(SOLID_REACTIVE_MULTINEXUS),
        Some(SPLASH),
        Some(MULTISPLASH),
        Some(SOLID_SPLASH),
        Some(SOLID_MULTISPLASH),
        Some(PIXEL_RAIN),
        Some(PIXEL_FRACTAL),
    ]
};

/// The VialRGB id of our effect `mode` (Off for one Vial lacks).
pub fn to_vial(mode: u8) -> u16 {
    EFFECTS
        .iter()
        .position(|&e| e == Some(mode))
        .map_or(OFF, |i| i as u16)
}

/// Our effect for VialRGB id `id`, if we have it.
pub fn from_vial(id: u16) -> Option<u8> {
    *EFFECTS.get(id as usize)?
}

/// set_mode (vialrgb.c, with the _noeeprom setters): `id`, then speed, h,
/// s, v. Off while an effect Vial has no id for is on can't be Vial's effect
/// list (it already shows Off, so picking Off changes nothing): it's a
/// slider, and the effect stays.
pub fn set_mode(e: &mut Engine, id: u16, speed: u8, h: u8, s: u8, v: u8) {
    let shown_off = e.config.enable && to_vial(e.config.mode) == OFF;
    if id == OFF && !shown_off {
        e.disable();
    } else {
        e.enable();
        // An unknown id: QMK's mode 0, which rgb_matrix ignores.
        if let Some(mode) = from_vial(id) {
            e.set_mode(mode);
        }
    }
    e.set_speed(speed);
    e.sethsv(h, s, v);
}

/// get_supported: the ids above `gt`, u16 little-endian from the start
/// of `out`, the rest 0xFF.
pub fn supported_after(gt: u16, out: &mut [u8]) {
    out.fill(0xFF);
    let ids = (gt.saturating_add(1)..EFFECTS.len() as u16).filter(|&i| from_vial(i).is_some());
    for (slot, id) in out.chunks_exact_mut(2).zip(ids) {
        slot.copy_from_slice(&id.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for mode in 1..effect::RGBR_PLAY {
            match to_vial(mode) {
                OFF => assert_eq!(mode, effect::PIXEL_FLOW),
                id => assert_eq!(from_vial(id), Some(mode)),
            }
        }
        assert_eq!(to_vial(effect::RGBR_PLAY), OFF);
        assert_eq!(to_vial(effect::CYCLE_LEFT_RIGHT), 14);
        assert_eq!(to_vial(effect::PIXEL_FRACTAL), 44);
        assert_eq!(from_vial(1), None);
        assert_eq!(from_vial(500), None);
    }

    #[test]
    fn a_slider_keeps_an_effect_vial_lacks() {
        use crate::Config;
        let mut e = Engine::new(Config::default());
        e.set_mode(effect::PIXEL_FLOW);
        // A slider: Off (what Vial read), with the new brightness.
        set_mode(&mut e, OFF, 100, 10, 20, 30);
        assert!(e.config.enable);
        assert_eq!(e.config.mode, effect::PIXEL_FLOW);
        assert_eq!((e.config.speed, e.config.hsv.v), (100, 30));
        // Another effect, then Off from the list: off.
        set_mode(&mut e, to_vial(effect::BREATHING), 100, 10, 20, 30);
        assert_eq!(e.config.mode, effect::BREATHING);
        set_mode(&mut e, OFF, 100, 10, 20, 30);
        assert!(!e.config.enable);
        // Off, then an effect: on again.
        set_mode(&mut e, to_vial(effect::SOLID_COLOR), 100, 10, 20, 30);
        assert!(e.config.enable);
        assert_eq!(e.config.mode, effect::SOLID_COLOR);
    }

    #[test]
    fn supported_pages() {
        extern crate std;
        let mut out = [0; 30];
        supported_after(40, &mut out);
        assert_eq!(&out[..10], &[41, 0, 42, 0, 43, 0, 44, 0, 0xFF, 0xFF]);
        // vial-gui's walk: from 0, then after the last id of each reply.
        let mut all = std::vec::Vec::new();
        loop {
            supported_after(all.last().copied().unwrap_or(0), &mut out);
            let n = all.len();
            all.extend(
                out.chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&i| i != 0xFFFF),
            );
            if all.len() == n {
                break;
            }
        }
        assert_eq!(all, (2..45).collect::<std::vec::Vec<u16>>());
    }
}
