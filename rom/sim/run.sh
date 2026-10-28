#!/usr/bin/env bash
# Runs the ROM bootloader suites (rom/sim/tests/*.robot, or the ones given)
# in Renode, in parallel, with results in rom/sim/results/. The ROM is
# $ROM_WORK/dump/rom.bin (rom/README.md). peek.robot also needs akko-rmk
# with the rom-peek feature, built here into target/rom-peek (never into
# target/, which the board sim uses).
#   rom/sim/run.sh                          all suites
#   rom/sim/run.sh rom/sim/tests/x.robot    one suite
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tools=$(cd "$root/.." && pwd)/tools
RENODE=${RENODE:-$tools/renode}
VENV=${RENODE_VENV:-$tools/renode-venv}
[ $# -gt 0 ] || set -- "$here"/tests/*.robot
suites=()
for s in "$@"; do suites+=("$(realpath "$s")"); done
if printf '%s\n' "${suites[@]}" | grep -q '/peek\.robot$'; then
    (cd "$root" && CARGO_TARGET_DIR=target/rom-peek cargo build --release --bin akko-rmk --features rom-peek)
fi
out=$here/results
rm -rf "$out" && mkdir -p "$out/logs" "$out/snapshots" && cd "$out"
PATH=$VENV/bin:$PATH "$RENODE/renode-test" -j "${#suites[@]}" "${suites[@]}"
