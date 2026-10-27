#!/usr/bin/env bash
# Build the firmware images and run the Renode simulation tests.
#   sim/run.sh                       all suites
#   sim/run.sh sim/tests/x.robot     one suite
#   SIM_MATCH='vial|knob' sim/run.sh only tests whose name matches (regex, any case)
#   SIM_JOBS=4 sim/run.sh            Renode instances at once (default: half the
#                                    CPUs, at most 16)
# Every test runs as its own suite (sim/shard.py), so they run in parallel.
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
cargo build --release --bin akko-rmk
# m2-blink again with the time base started just before its counter's
# half-wrap and full wrap.
for b in half wrap; do
    CARGO_TARGET_DIR=target/sim-near-$b cargo build --release --bin m2-blink --features sim-time-near-$b
done

if [ $# -eq 0 ]; then
    set -- sim/tests/*.robot
fi
shards=$PWD/target/sim-shards
rm -rf "$shards"
mapfile -t suites < <(python3 sim/shard.py "$shards" ${SIM_MATCH:+--match "$SIM_MATCH"} "$@")
cpus=$(nproc)
jobs=${SIM_JOBS:-$(( cpus / 2 < 16 ? (cpus / 2 > 0 ? cpus / 2 : 1) : 16 ))}

# renode-test writes its reports, logs and failure snapshots to the current directory.
out=target/sim-results
rm -rf "$out" && mkdir -p "$out" && cd "$out"
PATH=$VENV/bin:$PATH "$RENODE/renode-test" -j "$jobs" "${suites[@]}"
