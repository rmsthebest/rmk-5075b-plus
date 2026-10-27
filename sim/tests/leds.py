"""Robot helpers for the LED tests: QMK's colour maths, and the LED layout
from lighting/src/layout.rs."""

import re
from pathlib import Path

_LAYOUT = (Path(__file__).resolve().parent / "../../lighting/src/layout.rs").read_text()
POINTS = [(int(x), int(y)) for x, y in
          re.findall(r"\((\d+), (\d+)\)", _LAYOUT.split("pub const POINTS")[1].split("];")[0])]


def hsv_to_rgb(h, s, v):
    """QMK's hsv_to_rgb (no CIE curve), as 0xRRGGBB."""
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
    """The effect time t for which every LED shows CYCLE_LEFT_RIGHT's colour,
    hue = x - t, or fail."""
    colours = [int(c) for c in colours]
    for t in range(256):
        if all(c == hsv_to_rgb((x - t) & 0xFF, s, v) for c, (x, _) in zip(colours, POINTS)):
            return t
    raise AssertionError("no time fits cycle_left_right: %s" % " ".join("%06x" % c for c in colours))


def lit(colours):
    """Indices of the LEDs that aren't off."""
    return [i for i, c in enumerate(colours) if int(c)]
