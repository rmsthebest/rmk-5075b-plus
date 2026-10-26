#!/usr/bin/env python3
"""Generate akko-5075b-plus/src/keymap.rs from the QMK VIA keymap.

    python3 -I tools/gen_keymap.py [qmk-keyboard-dir] > akko-5075b-plus/src/keymap.rs

Reads keymaps/via/keymap.c (the layers and the encoder map) and keyboard.json
(LAYOUT_iso's key order -> matrix positions) from the QMK keyboard directory,
default ../qmk_5075b_plus/keyboards/akko/5075b_plus. Every QMK keycode the
keymap uses must be in KEYCODES below, so an unknown one fails the run rather
than turning into a silent No.

The Win/Mac switch: src/scan.rs taps a matrix position that isn't on the
board, (5,3) for Windows or (5,4) for Mac, whenever the switch moves (and at
start-up); every layer has DF(_BL) / DF(_MBL) there, as QMK's housekeeping
sets the default layer from the switch. Vial doesn't show those positions.

The board's own keycodes (keyboard.json "keycodes", QK_KB_0 + n) become
User(n), which RMK's Vial code maps to the same 0x7E00 + n, so Vial shows the
same custom keycodes as VIA. Until their milestones they do nothing.
"""

import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
QMK_DIR = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "../../qmk_5075b_plus/keyboards/akko/5075b_plus")

# QMK keycode -> Rust expression (inside keymap.rs, where the rmk macros and
# the helpers at the top are in scope).
KEYCODES = {
    "_______": "a!(Transparent)",
    "XXXXXXX": "a!(No)",
    "KC_ESC": "k!(Escape)", "KC_GRV": "k!(Grave)", "KC_MINS": "k!(Minus)", "KC_EQL": "k!(Equal)",
    "KC_BSPC": "k!(Backspace)", "KC_DEL": "k!(Delete)", "KC_TAB": "k!(Tab)", "KC_LBRC": "k!(LeftBracket)",
    "KC_RBRC": "k!(RightBracket)", "KC_ENT": "k!(Enter)", "KC_CAPS": "k!(CapsLock)", "KC_SCLN": "k!(Semicolon)",
    "KC_QUOT": "k!(Quote)", "KC_BSLS": "k!(Backslash)", "KC_NUBS": "k!(NonusBackslash)", "KC_COMM": "k!(Comma)",
    "KC_DOT": "k!(Dot)", "KC_SLSH": "k!(Slash)", "KC_SPC": "k!(Space)",
    "KC_LSFT": "k!(LShift)", "KC_RSFT": "k!(RShift)", "KC_LCTL": "k!(LCtrl)", "KC_RCTL": "k!(RCtrl)",
    "KC_LALT": "k!(LAlt)", "KC_RALT": "k!(RAlt)", "KC_LGUI": "k!(LGui)", "KC_RGUI": "k!(RGui)",
    "KC_UP": "k!(Up)", "KC_DOWN": "k!(Down)", "KC_LEFT": "k!(Left)", "KC_RGHT": "k!(Right)",
    "KC_HOME": "k!(Home)", "KC_END": "k!(End)", "KC_PGUP": "k!(PageUp)", "KC_PGDN": "k!(PageDown)",
    "KC_INS": "k!(Insert)", "KC_PSCR": "k!(PrintScreen)",
    "KC_MUTE": "k!(AudioMute)", "KC_VOLD": "k!(AudioVolDown)", "KC_VOLU": "k!(AudioVolUp)",
    "KC_MPLY": "k!(MediaPlayPause)", "KC_MPRV": "k!(MediaPrevTrack)", "KC_MNXT": "k!(MediaNextTrack)",
    "KC_MSEL": "k!(MediaSelect)", "KC_MYCM": "k!(MyComputer)", "KC_MAIL": "k!(Mail)",
    "KC_WSCH": "k!(WwwSearch)", "KC_WHOM": "k!(WwwHome)", "KC_CALC": "k!(Calculator)",
    "KC_BRID": "k!(BrightnessDown)", "KC_BRIU": "k!(BrightnessUp)", "KC_MCTL": "k!(MissionControl)",
    "RGB_MOD": "light(LightAction::RgbModeForward)", "RGB_TOG": "light(LightAction::RgbTog)",
    "RGB_SPI": "light(LightAction::RgbSpi)", "RGB_SPD": "light(LightAction::RgbSpd)",
    "RGB_VAI": "light(LightAction::RgbVai)", "RGB_VAD": "light(LightAction::RgbVad)",
    "RGB_SAI": "light(LightAction::RgbSai)", "RGB_SAD": "light(LightAction::RgbSad)",
    # QK_BOOT: bootloader.rs turns its press into boot-guard's DFU request.
    "QK_BOOT": "kbctrl(KeyboardAction::Bootloader)",
    # QMK's EE_CLR clears the settings after a 3 s hold (5075b_plus.c); RMK's
    # ClearEeprom would act at once. No-op until that hold is ported.
    "EE_CLR": "a!(No)",
    # NKRO and the GUI lock aren't in RMK yet.
    "NK_TOGG": "a!(No)",
    "GU_TOGG": "a!(No)",
}
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    KEYCODES["KC_" + c] = "k!(%s)" % c
for d in "1234567890":
    KEYCODES["KC_" + d] = "k!(Kc%s)" % d
for f in range(1, 13):
    KEYCODES["KC_F%d" % f] = "k!(F%d)" % f


# Virtual positions for the Win/Mac switch (see the docstring).
SWITCH_KEYS = {(5, 3): "_BL", (5, 4): "_MBL"}


def strip_comments(src):
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    return re.sub(r"//[^\n]*", "", src)


def split_args(s):
    """Split on top-level commas."""
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


def body(src, start):
    """The text inside the parentheses that open at or after src[start]."""
    i = src.index("(", start)
    depth = 0
    for j in range(i, len(src)):
        if src[j] == "(":
            depth += 1
        elif src[j] == ")":
            depth -= 1
            if depth == 0:
                return src[i + 1:j]
    raise ValueError("unbalanced parentheses")


def main():
    kb = json.load(open(os.path.join(QMK_DIR, "keyboard.json")))
    rows, cols = len(kb["matrix_pins"]["rows"]), len(kb["matrix_pins"]["cols"])
    order = [tuple(k["matrix"]) for k in kb["layouts"]["LAYOUT_iso"]["layout"]]
    for i, k in enumerate(kb["keycodes"]):
        KEYCODES.setdefault(k["key"], "user(%d)" % i)

    src = strip_comments(open(os.path.join(QMK_DIR, "keymaps/via/keymap.c")).read())
    enum = re.search(r"enum\s+layers\s*\{(.*?)\}", src, re.S).group(1)
    layer_names = [n.strip().split("=")[0].strip() for n in enum.split(",") if n.strip()]
    layer_index = dict((n, i) for i, n in enumerate(layer_names))
    for n, i in list(layer_index.items()):
        KEYCODES["MO(%s)" % n] = "mo!(%d)" % i

    km = src[src.index("keymaps[]"):src.index("rgbrec_default_effects")]
    layers = {}
    for m in re.finditer(r"\[(\w+)\]\s*=\s*LAYOUT_iso", km):
        keys = split_args(body(km, m.end() - 1))
        if len(keys) != len(order):
            sys.exit("layer %s has %d keys, LAYOUT_iso has %d" % (m.group(1), len(keys), len(order)))
        layers[layer_index[m.group(1)]] = keys

    enc = src[src.index("encoder_map"):]
    encoders = {}
    for m in re.finditer(r"\[(\w+)\]\s*=\s*\{\s*ENCODER_CCW_CW\(([^)]*)\)\s*\}", enc):
        ccw, cw = [x.strip() for x in m.group(2).split(",")]
        encoders[layer_index[m.group(1)]] = (cw, ccw)

    def rust(code):
        if code not in KEYCODES:
            sys.exit("no RMK mapping for QMK keycode %s (add it to KEYCODES)" % code)
        return KEYCODES[code]

    out = []
    w = out.append
    w("// Generated by tools/gen_keymap.py from the QMK VIA keymap")
    w("// (keyboards/akko/5075b_plus/keymaps/via/keymap.c). Don't edit; rerun it.")
    w("")
    w("use rmk::types::action::{Action, EncoderAction, KeyAction, KeyboardAction, LightAction};")
    w("use rmk::{a, k, mo};")
    w("")
    w("pub const ROW: usize = %d;" % rows)
    w("pub const COL: usize = %d;" % cols)
    w("pub const NUM_LAYER: usize = %d;" % len(layers))
    w("pub const NUM_ENCODER: usize = 1;")
    w("")
    w("const fn light(a: LightAction) -> KeyAction {")
    w("    KeyAction::Single(Action::Light(a))")
    w("}")
    w("")
    w("const fn user(n: u8) -> KeyAction {")
    w("    KeyAction::Single(Action::User(n))")
    w("}")
    w("")
    w("const fn df(layer: u8) -> KeyAction {")
    w("    KeyAction::Single(Action::DefaultLayer(layer))")
    w("}")
    w("")
    w("/// Matrix positions the Win/Mac switch taps (see src/scan.rs).")
    for (r, c), layer in sorted(SWITCH_KEYS.items()):
        w("pub const %s: (u8, u8) = (%d, %d);" % ("SWITCH_WIN" if layer == "_BL" else "SWITCH_MAC", r, c))
    w("")
    w("const fn kbctrl(a: KeyboardAction) -> KeyAction {")
    w("    KeyAction::Single(Action::KeyboardControl(a))")
    w("}")
    w("")
    w("#[rustfmt::skip]")
    w("pub const KEYMAP: [[[KeyAction; COL]; ROW]; NUM_LAYER] = [")
    for li in range(len(layers)):
        grid = [["a!(No)"] * cols for _ in range(rows)]
        for (r, c), code in zip(order, layers[li]):
            grid[r][c] = rust(code)
        for (r, c), layer in SWITCH_KEYS.items():
            if (r, c) in order:
                sys.exit("switch key %d,%d is on the board" % (r, c))
            grid[r][c] = "df(%d)" % layer_index[layer]
        w("    // %d: %s" % (li, layer_names[li]))
        w("    [")
        for r in range(rows):
            w("        [" + ", ".join(grid[r]) + "],")
        w("    ],")
    w("];")
    w("")
    w("/// Clockwise, counter-clockwise per layer.")
    w("#[rustfmt::skip]")
    w("pub const ENCODER_MAP: [[EncoderAction; NUM_ENCODER]; NUM_LAYER] = [")
    for li in range(len(layers)):
        cw, ccw = encoders.get(li, ("_______", "_______"))
        w("    [EncoderAction::new(%s, %s)]," % (rust(cw), rust(ccw)))
    w("];")
    print("\n".join(out))


if __name__ == "__main__":
    main()
