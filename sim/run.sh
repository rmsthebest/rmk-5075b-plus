#!/usr/bin/env bash
# Build the firmware images and run the Renode simulation tests.
#   sim/run.sh                       all suites
#   sim/run.sh sim/tests/x.robot     one suite
#   SIM_MATCH='vial|knob' sim/run.sh only tests whose name matches (regex, any case)
#   SIM_CHANGED=1 sim/run.sh         only the suites the uncommitted changes can
#                                    affect (sim/affected.py; SIM_CHANGED=<rev>:
#                                    the changes since <rev>). Not for an image
#                                    about to be flashed: that runs everything.
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
# The `mac` feature's image, in its own target dir so it doesn't replace the
# default one (m4_rmk's Mac tests boot it).
cargo build --release --bin akko-rmk --features mac --target-dir target/mac

if [ $# -eq 0 ]; then
    set -- sim/tests/*.robot
fi
shards=$PWD/target/sim-shards
# How long each test took last time (sim/durations.py), so shard.py starts
# the longest first. In the main checkout's target/, shared by worktrees.
durations=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/target/sim-durations.json
rm -rf "$shards"
if [ -n "${SIM_CHANGED:-}" ]; then
    rev=$([ "$SIM_CHANGED" = 1 ] && echo HEAD || echo "$SIM_CHANGED")
    suites=()
    while IFS=$'\t' read -r suite match; do
        echo "sim: ${suite##*/} ${match:+(only: $match)}"
        mapfile -t -O "${#suites[@]}" suites < <(python3 sim/shard.py "$shards" --durations "$durations" ${match:+--match "$match"} "$suite")
    done < <(python3 sim/affected.py "$rev")
    if [ ${#suites[@]} -eq 0 ]; then
        echo "sim: no suite affected"
        exit 0
    fi
else
    mapfile -t suites < <(python3 sim/shard.py "$shards" --durations "$durations" ${SIM_MATCH:+--match "$SIM_MATCH"} "$@")
fi
cpus=$(nproc)
jobs=${SIM_JOBS:-$(( cpus / 2 < 16 ? (cpus / 2 > 0 ? cpus / 2 : 1) : 16 ))}

# renode-test writes its reports, logs and failure snapshots to the current directory.
out=target/sim-results
# logs/ and snapshots/ up front: renode-keywords.robot creates them per test
# (check, then create), and two shards starting together can race.
root=$PWD
rm -rf "$out" && mkdir -p "$out/logs" "$out/snapshots" && cd "$out"
status=0
PATH=$VENV/bin:$PATH "$RENODE/renode-test" -j "$jobs" "${suites[@]}" || status=$?
python3 "$root/sim/durations.py" robot_output.xml "$durations" || true
exit $status
