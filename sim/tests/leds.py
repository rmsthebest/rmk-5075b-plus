"""Robot helpers for the LED tests: QMK's colour maths, and the LED layout
from crates/akko-lighting/src/layout.rs."""

import re
from pathlib import Path

_LAYOUT = (Path(__file__).resolve().parent / "../../crates/akko-lighting/src/layout.rs").read_text()
POINTS = [(int(x), int(y)) for x, y in
          re.findall(r"\((\d+), (\d+)\)", _LAYOUT.split("pub const POINTS")[1].split("];")[0])]
_CO = _LAYOUT.split("pub const MATRIX_CO:")[1].split("=", 1)[1].split("];")[0]
MATRIX_CO = [[None if v == "NO_LED" else int(v) for v in re.findall(r"NO_LED|\d+", row)]
             for row in re.findall(r"\[([^\[\]]*)\]", _CO)]
# The side lights (rgblight) are the last 22 LEDs; the rest are keys.
SIDE = range(84, 106)


def hsv_to_rgb(h, s, v):
    """QMK's hsv_to_rgb (no CIE curve), as 0xRRGGBB."""
    h, s, v = int(h), int(s), int(v)
    if s == 0:
        return (v << 16) | (v << 8) | v
    region = h * 6 // 255
    rem = ((h * 2 - region * 85) * 3) & 0xFF
    p = (v * (255 - s)) >> 8
    q = (v * (255 - ((s * rem) >> 8))) >> 8
    t = (v * (255 - ((s * (255 - rem)) >> 8))) >> 8
    r, g, b = {0: (v, t, p), 6: (v, t, p), 1: (q, v, p), 2: (p, v, t),
               3: (p, q, v), 4: (t, p, v)}.get(region, (v, p, q))
    return (r << 16) | (g << 8) | b


def cycle_left_right_time(colours, s=255, v=84):
    """The effect time t for which every key LED shows CYCLE_LEFT_RIGHT's
    colour, hue = x - t, or fail. (The side lights show their own effect;
    LED 0 is the battery's, dark unless the battery is low or charging.)"""
    colours = [int(c) for c in colours][:SIDE.start]
    for t in range(256):
        if all(c == hsv_to_rgb((x - t) & 0xFF, s, v) for c, (x, _) in list(zip(colours, POINTS))[1:]):
            return t
    raise AssertionError("no time fits cycle_left_right: %s" % " ".join("%06x" % c for c in colours))


def swirl_hue(colours, v=120):
    """The hue h for which the side lights show the vendor's rainbow swirl,
    light i (and 21 - i) at hue 11 i + h, or fail."""
    side = [int(c) for c in colours][SIDE.start:]
    for h in range(256):
        if all(side[i] == side[21 - i] == hsv_to_rgb((11 * i + h) & 0xFF, 255, v) for i in range(11)):
            return h
    raise AssertionError("the side lights aren't a swirl: %s" % " ".join("%06x" % c for c in side))


def side_colours(colours):
    """The side lights' colours, if they're all one colour, else fail."""
    side = {int(c) for c in colours[SIDE.start:]}
    if len(side) != 1:
        raise AssertionError("the side lights differ: %s" % " ".join("%06x" % int(c) for c in colours[SIDE.start:]))
    return side.pop()


def key_led(row, col):
    return MATRIX_CO[int(row)][int(col)]


def lit(colours):
    """Indices of the LEDs that aren't off."""
    return [i for i, c in enumerate(colours) if int(c)]


KEY_LEDS = {i for row in MATRIX_CO for i in row if i is not None and i < SIDE.start}


def lit_keys(colours):
    """Indices of the LEDs under keys that aren't off. (LED 0 has no key;
    recorded playback leaves it as it was.)"""
    return [i for i in lit(colours) if i in KEY_LEDS]
