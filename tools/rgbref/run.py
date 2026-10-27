#!/usr/bin/env python3
"""Differential test of the lighting crate against QMK's rgb_matrix C.

    python3 -I tools/rgbref/run.py [--qmk DIR] [--effect N ...] [--seed S]

Builds QMK's rgb_matrix.c, color.c and lib8tion (with tools/rgbref/stubs
for the rest of QMK, and g_led_config from the board's keyboard.json) into
target/rgbref/qmk-rgb, and the Rust side (lighting/examples/rgbref.rs) for
the host. Then runs the same scripts through both (see harness.c for the
format): every effect under several configs, with key presses, effect
changes, enable/flag toggles, suspend, and the timer near its 16-bit wrap.
Every flushed frame must match byte for byte: same time, same colours.
"""

import argparse
import json
import os
import random
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
OUT = ROOT / "target" / "rgbref"
EFFECTS = range(1, 46)  # 45, RGBR_PLAY, renders nothing in both here
OPS = ["toggle", "enable", "disable", "step", "step_reverse", "increase_hue", "decrease_hue",
       "increase_sat", "decrease_sat", "increase_val", "decrease_val", "increase_speed", "decrease_speed"]
ROWS, COLS = 6, 15


def gen_config(qmk_board):
    j = json.loads((qmk_board / "keyboard.json").read_text())
    rgb = j["rgb_matrix"]
    leds = rgb["layout"]
    h = ["#pragma once",
         f"#define MATRIX_ROWS {ROWS}", f"#define MATRIX_COLS {COLS}",
         f"#define RGB_MATRIX_LED_COUNT {len(leds)}",
         "#define RGB_MATRIX_WS2812",
         "#define RGB_MATRIX_SLEEP",
         f"#define RGB_MATRIX_MAXIMUM_BRIGHTNESS {rgb['max_brightness']}",
         # The board's config.h
         "#define RGB_MATRIX_FRAMEBUFFER_EFFECTS",
         "#define RGB_MATRIX_KEYPRESSES",
         "#define RGB_TRIGGER_ON_KEYDOWN",
         # The board's own effect, RGBR_PLAY (harness.c stubs its renderer)
         "#define RGB_MATRIX_CUSTOM_USER", "#define ENABLE_RGB_MATRIX_RGBR_PLAY",
         f"#define RGB_MATRIX_VAL_STEP {rgb['val_steps']}", f"#define RGB_MATRIX_SPD_STEP {rgb['speed_steps']}"]
    h += [f"#define ENABLE_RGB_MATRIX_{name.upper()}" for name, on in rgb["animations"].items() if on]
    (OUT / "config.h").write_text("\n".join(h) + "\n")

    co = [["NO_LED"] * COLS for _ in range(ROWS)]
    for i, l in enumerate(leds):
        if "matrix" in l:
            r, c = l["matrix"]
            co[r][c] = str(i)
    c = ['#include "rgb_matrix.h"', "led_config_t g_led_config = {", "    {"]
    c += ["        {" + ", ".join(row) + "}," for row in co]
    c += ["    },", "    {" + ", ".join(f"{{{l['x']}, {l['y']}}}" for l in leds) + "},",
          "    {" + ", ".join(str(l["flags"]) for l in leds) + "},", "};"]
    (OUT / "led_config.c").write_text("\n".join(c) + "\n")


def build(qmk):
    OUT.mkdir(parents=True, exist_ok=True)
    gen_config(qmk / "keyboards/akko/5075b_plus")
    q = qmk / "quantum"
    cmd = ["gcc", "-std=gnu11", "-O1", "-funsigned-char", "-fno-strict-aliasing", "-w",
           "-include", str(OUT / "config.h"), "-include", str(q / "rgb_matrix/post_config.h"),
           "-I", str(HERE / "stubs"), "-I", str(qmk / "keyboards/akko/5075b_plus"), "-I", str(q / "rgb_matrix"), "-I", str(q / "rgb_matrix/animations"),
           "-I", str(q / "rgb_matrix/animations/runners"), "-I", str(q), "-I", str(qmk),
           "-o", str(OUT / "qmk-rgb"),
           str(HERE / "harness.c"), str(OUT / "led_config.c"), str(q / "rgb_matrix/rgb_matrix.c"),
           str(q / "color.c"), str(qmk / "lib/lib8tion/lib8tion.c")]
    subprocess.run(cmd, check=True)
    subprocess.run(["cargo", "build", "-q", "-p", "lighting", "--example", "rgbref",
                    "--target", "x86_64-unknown-linux-gnu"], cwd=ROOT, check=True)
    return OUT / "qmk-rgb", ROOT / "target/x86_64-unknown-linux-gnu/debug/examples/rgbref"


def cfg(enable=1, mode=13, h=0, s=255, v=84, speed=153, flags=255):
    return f"cfg {enable} {mode} {h} {s} {v} {speed} {flags}"


def typing(rng, t0, t1, keys_per_s=8, step=1):
    """run/key lines from t0 to t1 with random presses and releases."""
    lines, t = [], t0
    down = []
    while t < t1:
        nxt = min(t1, t + rng.randint(1, 2000 // keys_per_s))
        lines.append(f"run {t} {nxt} {step}")
        t = nxt
        if down and rng.random() < 0.5:
            r, c = down.pop(rng.randrange(len(down)))
            lines.append(f"key {r} {c} 0")
        else:
            r, c = rng.randrange(ROWS), rng.randrange(COLS)
            down.append((r, c))
            lines.append(f"key {r} {c} 1")
    return lines


def scripts(effect, rng):
    """(name, lines) cases for one effect."""
    default = cfg(mode=effect)
    yield "default", [default, "run 0 3000 1"]
    yield "typing", [default] + typing(rng, 0, 4000)
    yield "burst", [default, "run 0 100 1"] + [f"key {r} {c} 1" for r, c in
                                               [(rng.randrange(ROWS), rng.randrange(COLS)) for _ in range(12)]] + ["run 100 2000 1"]
    for k in range(4):
        v = rng.choice([1, 84, 255, rng.randrange(1, 256)])
        if effect == 32 and v < 2:
            # Digital rain divides by v * 3 / 4: 0 on the Cortex-M3 (and in
            # the Rust), a crash on the host.
            v = 2
        c = cfg(mode=effect, h=rng.randrange(256), s=rng.choice([0, 255, rng.randrange(256)]),
                v=v, speed=rng.choice([0, 255, rng.randrange(256)]))
        step = rng.choice([1, 1, 2, 3, 7])
        yield f"cfg{k}", [c] + typing(rng, 0, 2500, step=step)
    # Timer across the 16-bit wrap, from a late start.
    yield "wrap", [default] + typing(rng, 64000, 68000)
    # Effect change in and out (init frames, statics that live on), enable
    # and flags toggles, suspend.
    other = rng.choice([e for e in EFFECTS if e != effect])
    # Setters. Digital rain (32) divides by zero at val 0 or 1, a crash on
    # the host, so "ops" keeps val at 2 or more and "ops_val" keeps away
    # from effect 32.
    for name, choices in [("ops", [o for o in OPS if o != "decrease_val"] + ["mode", "hsv", "speed"]),
                          ("ops_val", [o for o in OPS if o not in ("step", "step_reverse")] + ["hsv", "speed"])]:
        if name == "ops_val" and effect == 32:
            continue
        ops = [default, "run 0 200 1"]
        t = 200
        for _ in range(60):
            op = rng.choice(choices)
            if op == "mode":
                ops.append(f"mode {rng.choice([m for m in range(50) if m != 32])}")
            elif op == "hsv":
                ops.append(f"hsv {rng.randrange(256)} {rng.randrange(256)} {rng.randrange(2, 256)}")
            elif op == "speed":
                ops.append(f"speed {rng.randrange(256)}")
            else:
                ops.append(f"op {op}")
            ops.append(f"run {t} {t + rng.randint(1, 60)} 1")
            t = int(ops[-1].split()[2])
        yield name, ops
    yield "switch", [default, "run 0 500 1", cfg(mode=other), "run 500 900 1", default] + typing(rng, 900, 1800) + [
        cfg(enable=0, mode=effect), "run 1800 2000 1", default, "run 2000 2300 1",
        cfg(mode=effect, flags=2), "run 2300 2500 1", cfg(mode=effect, flags=4), "run 2500 2700 1",
        "suspend 1", "run 2700 2900 1", "suspend 0", "run 2900 3300 1"]


def first_diff(a, b):
    for n, (x, y) in enumerate(zip(a, b)):
        if x != y:
            tx, fx = x.split()
            ty, fy = y.split()
            if tx != ty:
                return f"frame {n}: QMK at {tx} ms, Rust at {ty} ms"
            led = next(i for i in range(len(fx) // 6) if fx[i * 6:i * 6 + 6] != fy[i * 6:i * 6 + 6])
            return f"frame {n} at {tx} ms, LED {led}: QMK {fx[led*6:led*6+6]}, Rust {fy[led*6:led*6+6]}"
    if len(a) != len(b):
        return f"QMK sent {len(a)} frames, Rust {len(b)}"
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--qmk", default=str(ROOT.parent / "qmk_5075b_plus"))
    ap.add_argument("--effect", type=int, nargs="*")
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    c_bin, rs_bin = build(Path(args.qmk))
    failed = total = 0
    for effect in args.effect or EFFECTS:
        rng = random.Random(args.seed * 1000 + effect)
        for name, lines in scripts(effect, rng):
            text = "\n".join(lines) + "\n"
            a = subprocess.run([c_bin], input=text, capture_output=True, text=True, check=True).stdout.splitlines()
            b = subprocess.run([rs_bin], input=text, capture_output=True, text=True, check=True).stdout.splitlines()
            total += 1
            d = first_diff(a, b)
            if d:
                failed += 1
                (OUT / f"fail-{effect}-{name}.txt").write_text(text)
                print(f"effect {effect} {name}: {d}  (script: target/rgbref/fail-{effect}-{name}.txt)")
    print(f"{total - failed}/{total} cases match")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
