#!/usr/bin/env bash
# Diffs an image's register-write trace in Renode (sim/trace.sh) against
# ours (akko-sim trace). Renode folds repeated lines into one with " (N)"
# for N more; those are expanded first. Its "FLASH ..." lines (the SPI
# flash's commands, in among the writes) are moved to the end, where ours
# prints them: the two streams are each compared in order.
#   sim-rs/compare/trace-diff.sh <image> [secs=0.5] [block-regex=.]
# <image> is a name from sim-rs/target/images. TIM2 isn't a Renode Python
# model, so its writes are left out of ours.
set -euo pipefail
cd "$(dirname "$0")/.."
image=${1:?usage: trace-diff.sh <image> [secs] [block-regex]}
secs=${2:-0.5}
filter=${3:-.}
out=target/trace-diff
mkdir -p "$out"
../sim/trace.sh "$PWD/target/images/$image" "$secs" "$filter" 2>"$out/$image.renode.err" \
    | awk '{ if (match($0, / \(([0-9]+)\)$/)) { n = substr($0, RSTART + 2, RLENGTH - 3); l = substr($0, 1, RSTART - 1); for (i = 0; i <= n; i++) print l } else print }' \
    >"$out/$image.renode.raw"
{ grep -v '^FLASH ' "$out/$image.renode.raw" || true; grep '^FLASH ' "$out/$image.renode.raw" || true; } >"$out/$image.renode"
cargo run --offline --release -q --bin akko-sim -- trace "$image" "$secs" "$filter" 2>"$out/$image.ours.err" \
    | grep -v '^TIM2' >"$out/$image.ours" || true
echo "Renode $(wc -l <"$out/$image.renode") writes, ours $(wc -l <"$out/$image.ours")"
diff "$out/$image.renode" "$out/$image.ours" && echo "same"
