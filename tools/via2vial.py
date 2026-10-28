#!/usr/bin/env python3
"""Write firmware/vial.json from the VIA definition.

    python3 -I tools/via2vial.py [via.json] > firmware/vial.json

Default input: ../akko_5075b_plus_via.json. Keeps what Vial uses: the name,
VID/PID, matrix, the KLE layout (with its encoder entries, "row,col\\n...\\ne")
and the custom keycodes, which RMK serves as User(0..15) (= QK_KB_0 + n, as in
QMK). VIA's lighting menus are dropped until M5 ports the lighting:
"lighting": "none".
"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
src = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "../../akko_5075b_plus_via.json")
via = json.load(open(src))

vial = {
    "name": via["name"],
    "vendorId": via["vendorId"],
    "productId": via["productId"],
    "lighting": "none",
    "matrix": via["matrix"],
    "customKeycodes": [{k: c[k] for k in ("name", "title", "shortName") if k in c} for c in via["customKeycodes"]],
    "layouts": {"keymap": via["layouts"]["keymap"]},
}
json.dump(vial, sys.stdout, indent=1)
print()
