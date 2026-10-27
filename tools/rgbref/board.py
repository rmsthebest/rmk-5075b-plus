#!/usr/bin/env python3
"""Differential test of the lighting crate's board module against the
vendor's C.

    python3 -I tools/rgbref/board.py [--qmk DIR] [--seed S] [--case NAME ...]

Builds the board's own lighting code into target/rgbref/qmk-board: the key,
blink and indicator functions of 5075b_plus.c (lifted out by name; the rest
of that file is wireless and battery code the harness doesn't model),
rgb_record/ and keymaps/via/keymap.c's default recordings, on top of QMK's
rgb_matrix.c, rgblight.c and process_rgb.c. The Rust side is
lighting/examples/boardref.rs. Both run the same scripts (format in
board_harness.c): boots, the RGB keys, side lights, recording, EE_CLR,
USB and the factory LED test, and random key sequences. Every flushed
frame, power pin change and state dump must match.
"""

import argparse
import json
import random
import re
import subprocess
import sys
from pathlib import Path

# -I leaves this directory off the path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402

HERE = run.HERE
ROOT = run.ROOT
OUT = run.OUT / "board"

# 5075b_plus.c's functions the harness takes as they are.
FUNCTIONS = [
    "hs_get_rgb_enable", "hs_set_rgb_enable", "eeconfig_confinfo_update", "eeconfig_confinfo_read",
    "eeconfig_confinfo_default", "eeconfig_confinfo_init", "process_record_hs", "im_rgblight_increase",
    "rgb_blink_dir", "hs_reset_settings_user", "nkr_indicators_hook", "rgb_matrix_hs_indicator_set",
    "rgb_matrix_hs_set_remain_time", "rgb_matrix_hs_indicator", "hs_set_color_all",
    "rgb_matrix_hs_devs_indicator", "hs_factory_scan", "keyboard_post_init_kb", "hs_reset_settings",
    "rgb_matrix_indicators_advanced_kb",
]
# process_record_kb's cases that are about lighting; the harness's
# process_record_kb has only these (the others are wireless, battery and
# key remapping).
CASES = ["KC_ENCW", "NK_TOGG", "RL_MOD", "EE_CLR", "RGB_SPI", "RGB_SPD", "RGB_VAI", "RGB_VAD", "RGB_SAI",
         "RGB_SAD", "RP_P0", "RP_P1", "RP_P2", "RP_END", "RGB_MOD", "RGB_HUI", "RGB_TOG"]
KEYS = ["RGB_TOG", "RGB_MOD", "RGB_RMOD", "RGB_HUI", "RGB_HUD", "RGB_SAI", "RGB_SAD", "RGB_VAI", "RGB_VAD",
        "RGB_SPI", "RGB_SPD", "RL_MOD", "RP_P0", "RP_P1", "RP_P2", "RP_END", "NK_TOGG", "EE_CLR", "KC_ENCW",
        "MO_FL", "MO_FBL", "KC_A"]


def braced(src, i):
    """src[i] is "{": the text through its matching "}"."""
    depth = 0
    for j in range(i, len(src)):
        if src[j] == "{":
            depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0:
                return src[i:j + 1]
    raise ValueError("unbalanced braces")


def function(src, name):
    m = re.search(r"^[A-Za-z][\w ]*\b%s\s*\([^()]*\)\s*\{" % name, src, re.M)
    if not m:
        raise ValueError("no function " + name)
    return src[m.start():m.end() - 1] + braced(src, m.end() - 1)


def case(body, name):
    m = re.search(r"case %s:\s*\{" % re.escape(name), body)
    if not m:
        raise ValueError("no case " + name)
    block = braced(body, m.end() - 1)
    return f"        case {name}: {block} break;"


def gen(qmk):
    board = qmk / "keyboards/akko/5075b_plus"
    OUT.mkdir(parents=True, exist_ok=True)
    run.OUT.mkdir(parents=True, exist_ok=True)
    run.gen_config(board)
    j = json.loads((board / "keyboard.json").read_text())

    d = j["rgb_matrix"]["default"]
    cfg = [(run.OUT / "config.h").read_text(),
           f"#define RGB_MATRIX_DEFAULT_MODE RGB_MATRIX_{d['animation'].upper()}",
           f"#define RGB_MATRIX_DEFAULT_SPD {d['speed']}", f"#define RGB_MATRIX_DEFAULT_VAL {d['val']}",
           "#define RGBLIGHT_ENABLE", "#define RGB_MATRIX_ENABLE", "#define EEPROM_ENABLE"]
    cfg += [f"#define RGBLIGHT_EFFECT_{name.upper()}" for name, on in j["rgblight"]["animations"].items() if on]
    cfg += [f'#include "{board / "config.h"}"']
    # The Rust keeps fewer blink slots than the vendor's 99; compare the
    # logic at the same count.
    blinks = re.search(r"const BLINKS: usize = (\d+);", (ROOT / "lighting/src/board.rs").read_text()).group(1)
    cfg += ["#undef HS_RGB_INDICATOR_COUNT", f"#define HS_RGB_INDICATOR_COUNT {blinks}"]
    (OUT / "config.h").write_text("\n".join(cfg) + "\n")

    order = [tuple(k["matrix"]) for k in j["layouts"]["LAYOUT_iso"]["layout"]]
    h = ["#pragma once", "#include \"quantum_keycodes.h\"", "enum board_keycodes {"]
    h += [f"    {k['key']} = QK_KB_0 + {i}," for i, k in enumerate(j["keycodes"])]
    h += ["};"]
    args = ", ".join(f"k{i}" for i in range(len(order)))
    grid = [["0"] * run.COLS for _ in range(run.ROWS)]
    for i, (r, c) in enumerate(order):
        grid[r][c] = f"k{i}"
    h += [f"#define LAYOUT_iso({args}) {{" + ", ".join("{" + ", ".join(row) + "}" for row in grid) + "}"]
    (OUT / "board_keycodes.h").write_text("\n".join(h) + "\n")

    src = (board / "5075b_plus.c").read_text()
    globals_ = src[src.index("typedef union {"):src.index("static uint32_t hs_factory_timer;")]
    globals_ += "static uint32_t hs_factory_timer;\n"
    kb = function(src, "process_record_kb")
    body = kb[kb.index("switch (keycode)"):]
    c = ['#include "quantum.h"', '#include "rgb_record.h"', '#include "rgb_rgblight.h"', "",
         "// Lifted from 5075b_plus.c by tools/rgbref/board.py.", globals_]
    funcs = [function(src, f) for f in FUNCTIONS]
    c += [f[:f.index("{")].strip() + ";" for f in funcs] + funcs
    c += ["bool process_record_kb(uint16_t keycode, keyrecord_t *record) {",
          "    if (process_record_user(keycode, record) != true) return false;",
          "    if (process_record_hs(keycode, record) != true) return false;",
          "    switch (keycode) {"] + [case(body, n) for n in CASES] + ["        default: break;", "    }",
                                                                       "    return true;", "}"]
    keymap = (board / "keymaps/via/keymap.c").read_text()
    i = keymap.index("const uint16_t PROGMEM rgbrec_default_effects")
    c += ["", keymap[i:keymap.index("};", i) + 2]]
    (OUT / "board_gen.c").write_text("\n".join(c) + "\n")


def build(qmk):
    gen(qmk)
    q = qmk / "quantum"
    board = qmk / "keyboards/akko/5075b_plus"
    cmd = ["gcc", "-std=gnu11", "-O1", "-funsigned-char", "-fno-strict-aliasing", "-w",
           "-include", str(OUT / "config.h"), "-include", str(q / "rgb_matrix/post_config.h"),
           "-I", str(OUT), "-I", str(HERE / "board"), "-I", str(HERE / "stubs"), "-I", str(board),
           "-I", str(board / "rgb_record"), "-I", str(q / "rgb_matrix"), "-I", str(q / "rgb_matrix/animations"),
           "-I", str(q / "rgb_matrix/animations/runners"), "-I", str(q / "rgblight"), "-I", str(q / "process_keycode"),
           "-I", str(q / "keymap_extras"), "-I", str(q / "sequencer"), "-I", str(q), "-I", str(qmk), "-include", "string.h",
           "-o", str(run.OUT / "qmk-board"),
           str(HERE / "board_harness.c"), str(OUT / "board_gen.c"), str(run.OUT / "led_config.c"),
           str(q / "rgb_matrix/rgb_matrix.c"), str(q / "color.c"), str(qmk / "lib/lib8tion/lib8tion.c"),
           str(q / "rgblight/rgblight.c"), str(q / "process_keycode/process_rgb.c"),
           str(board / "rgb_record/rgb_record.c"), str(board / "rgb_record/rgb_effect.c"),
           str(board / "rgb_record/rgb_rgblight.c")]
    subprocess.run(cmd, check=True)
    subprocess.run(["cargo", "build", "-q", "-p", "lighting", "--example", "boardref",
                    "--target", "x86_64-unknown-linux-gnu"], cwd=ROOT, check=True)
    return run.OUT / "qmk-board", ROOT / "target/x86_64-unknown-linux-gnu/debug/examples/boardref"


# Key positions: the keymap's, where the key has one (Fn layer), else a
# letter's.
POS = {"RGB_TOG": (3, 9), "RGB_MOD": (0, 13), "RGB_VAI": (4, 13), "RGB_VAD": (5, 13), "RGB_SAI": (5, 12),
       "RGB_SAD": (5, 14), "RGB_SPI": (1, 12), "RGB_SPD": (1, 11), "RL_MOD": (5, 0), "NK_TOGG": (4, 6),
       "EE_CLR": (1, 0), "KC_ENCW": (3, 5), "MO_FL": (5, 10), "MO_FBL": (4, 12)}


class Script:
    def __init__(self, rng, lctl=0, esc=0):
        self.rng = rng
        self.t = 0
        self.lines = [f"boot {lctl} {esc}"]

    def run(self, ms):
        self.lines.append(f"run {self.t} {self.t + ms} 1")
        self.t += ms

    def key(self, name, pressed, shift=0, pos=None):
        r, c = pos or POS.get(name) or (self.rng.randrange(run.ROWS), self.rng.randrange(run.COLS))
        self.lines.append(f"key {name} {r} {c} {int(pressed)} {shift}")

    def tap(self, name, shift=0, hold=30, gap=None, pos=None):
        pos = pos or POS.get(name) or (self.rng.randrange(run.ROWS), self.rng.randrange(run.COLS))
        self.key(name, 1, shift, pos)
        self.run(hold)
        self.key(name, 0, shift, pos)
        self.run(gap if gap is not None else self.rng.randint(30, 400))

    def dump(self):
        self.lines.append("dump")


def scripts(rng):
    s = Script(rng)
    s.run(6000)
    s.dump()
    yield "idle", s

    s = Script(rng)
    s.run(1000)
    for _ in range(20):
        s.tap("RGB_MOD", gap=rng.choice([60, 300, 700]))
    s.run(1000)
    s.dump()
    yield "mod_cycle", s

    s = Script(rng)
    s.run(500)
    for _ in range(40):
        s.tap(rng.choice(["RGB_SAI", "RGB_SAD", "RGB_HUI", "RGB_MOD", "RGB_SAI", "RGB_SAD"]))
    s.dump()
    yield "colours", s

    s = Script(rng)
    s.run(500)
    for _ in range(25):
        s.tap("RL_MOD", gap=rng.choice([100, 1500, 3000]))
        if rng.random() < 0.15:
            s.tap("RGB_TOG", gap=500)
    s.dump()
    yield "side", s

    s = Script(rng)
    s.run(500)
    for _ in range(30):
        s.tap(rng.choice(["RGB_TOG", "RL_MOD", "RGB_VAD", "RGB_VAI"]), gap=rng.choice([50, 400]))
    s.dump()
    yield "toggle", s

    s = Script(rng)
    s.run(500)
    for _ in range(50):
        s.tap(rng.choice(["RGB_VAI", "RGB_VAD", "RGB_SPI", "RGB_SPD", "RGB_HUD", "RGB_RMOD"]),
              shift=int(rng.random() < 0.3), gap=rng.choice([40, 300]))
    s.dump()
    yield "val_spd", s

    s = Script(rng)
    s.run(500)
    s.tap("RP_END")  # not in playback: nothing
    for ch in ["RP_P1", "RP_P0", "RP_P2"]:
        s.tap(ch, gap=600)
    s.tap("RP_END")
    for _ in range(30):
        name = rng.choice(["KC_A", "KC_A", "KC_A", "MO_FL", "MO_FBL", "RGB_SAI", "RL_MOD", "RP_P1"])
        s.tap(name, gap=rng.choice([30, 200]))
    s.tap("RP_END", gap=500)
    s.dump()
    s.tap("RP_END")
    for _ in range(6):
        s.tap("KC_A")
    s.tap("RGB_MOD", gap=500)  # leaving playback while recording drops it
    s.tap("RP_P0", gap=500)
    s.tap("RGB_MOD", gap=500)
    s.dump()
    yield "record", s

    s = Script(rng)
    s.run(500)
    for name in ["RGB_MOD", "RGB_SAI", "RL_MOD", "RL_MOD", "RGB_VAD", "RP_P2"]:
        s.tap(name, gap=200)
    s.key("EE_CLR", 1)
    s.run(3500)
    s.key("EE_CLR", 0)
    s.run(2000)
    s.dump()
    s.key("EE_CLR", 1)
    s.run(1000)
    s.key("EE_CLR", 0)
    s.run(3000)
    s.dump()
    yield "ee_clr", s

    s = Script(rng)
    s.lines.insert(0, "usb 0")
    s.run(3200)
    s.lines.append("usb 1")
    s.run(3000)
    s.lines.append("usb 0")
    s.run(1700)
    s.lines.append("usb 1")
    s.run(2500)
    yield "usb", s

    for lctl, esc in [(1, 0), (1, 1)]:
        s = Script(rng, lctl, esc)
        s.run(19000)
        yield f"factory{lctl}{esc}", s

    for n in range(6):
        s = Script(rng)
        s.run(rng.randint(0, 500))
        for _ in range(60):
            name = rng.choice(KEYS)
            if name == "EE_CLR" and rng.random() < 0.7:
                continue
            s.tap(name, shift=int(rng.random() < 0.15), hold=rng.choice([20, 30, 3200 if name == "EE_CLR" else 80]),
                  gap=rng.choice([20, 100, 500, 1200]))
        s.dump()
        yield f"random{n}", s


def first_diff(a, b):
    for n, (x, y) in enumerate(zip(a, b)):
        if x != y:
            if x[0] in "PD" or y[0] in "PD":
                return f"line {n}: QMK {x[:120]!r}, Rust {y[:120]!r}"
            tx, fx = x.split()
            ty, fy = y.split()
            if tx != ty:
                return f"line {n}: QMK frame at {tx} ms, Rust at {ty} ms"
            led = next(i for i in range(len(fx) // 6) if fx[i * 6:i * 6 + 6] != fy[i * 6:i * 6 + 6])
            return f"line {n}, frame at {tx} ms, LED {led}: QMK {fx[led*6:led*6+6]}, Rust {fy[led*6:led*6+6]}"
    if len(a) != len(b):
        return f"QMK printed {len(a)} lines, Rust {len(b)}"
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--qmk", default=str(ROOT.parent / "qmk_5075b_plus"))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--case", nargs="*")
    args = ap.parse_args()
    c_bin, rs_bin = build(Path(args.qmk))
    defaults = subprocess.run([c_bin, "defaults"], capture_output=True, text=True, check=True).stdout.strip()
    failed = total = 0
    for name, s in scripts(random.Random(args.seed)):
        if args.case and name not in args.case:
            continue
        text = "\n".join(s.lines) + "\n"
        a = subprocess.run([c_bin], input=text, capture_output=True, text=True, check=True).stdout.splitlines()
        b = subprocess.run([rs_bin], input=f"defaults {defaults}\n" + text, capture_output=True, text=True,
                           check=True).stdout.splitlines()
        total += 1
        d = first_diff(a, b)
        if d:
            failed += 1
            (run.OUT / f"board-fail-{name}.txt").write_text(text)
            print(f"{name}: {d}  (script: target/rgbref/board-fail-{name}.txt)")
    print(f"{total - failed}/{total} cases match")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
