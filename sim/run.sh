#!/usr/bin/env bash
# Build the firmware images and run the Renode simulation tests.
#   sim/run.sh                       all suites
#   sim/run.sh sim/tests/x.robot     one suite (extra args go to renode-test)
# Needs Renode in ../tools/renode and its test deps in ../tools/renode-venv
# (see sim/README.md).
set -euo pipefail
cd "$(dirname "$0")/.."

TOOLS=$(cd .. && pwd)/tools
RENODE=${RENODE:-$TOOLS/renode}
VENV=${RENODE_VENV:-$TOOLS/renode-venv}

# m1a needs boot-guard's always-dfu feature, so it's built on its own.
cargo build --release --bin m1b-stub
cargo build --release --bin m1a-bounce --features always-dfu
cargo build --release --bin m2-blink
cargo build --release --bin m3-usb
# m2-blink again with the time base started just before its counter's
# half-wrap and full wrap.
for b in half wrap; do
    CARGO_TARGET_DIR=target/sim-near-$b cargo build --release --bin m2-blink --features sim-time-near-$b
done

if [ $# -eq 0 ]; then
    set -- sim/tests/*.robot
fi
suites=()
for s in "$@"; do suites+=("$(realpath "$s")"); done

# renode-test writes its reports, logs and failure snapshots to the current directory.
out=target/sim-results
rm -rf "$out" && mkdir -p "$out" && cd "$out"
PATH=$VENV/bin:$PATH "$RENODE/renode-test" "${suites[@]}"
