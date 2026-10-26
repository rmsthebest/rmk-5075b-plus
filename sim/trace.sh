#!/usr/bin/env bash
# Run an image in the simulator and print its register writes in order, one per
# line ("BLOCK+0xOFF = 0xVALUE"), without timestamps, so two images can be diffed.
#   sim/trace.sh <image.elf|image.bin> [seconds=0.5] [block-regex=.]
# e.g. the QMK clock-setup reference:
#   sim/trace.sh ../akko_5075b_plus_fixes.bin 0.5 '^(PWR|ANCTL|RCC)'
# Also prints any LOCKED writes or accesses to unmodelled addresses to stderr.
set -euo pipefail
cd "$(dirname "$0")/.."

image=$(realpath "${1:?usage: sim/trace.sh <image> [seconds] [block-regex]}")
secs=${2:-0.5}
filter=${3:-.}
RENODE=${RENODE:-$(cd .. && pwd)/tools/renode}

case "$image" in
    *.bin) load="sysbus LoadBinary @$image 0x08000000" ;;
    *)     load="sysbus LoadELF @$image" ;;
esac

log=$(mktemp)
trap 'rm -f "$log"' EXIT
"$RENODE/renode" --console --disable-gui --plain \
    -e "include @sim/akko.resc; $load; machine Reset; emulation RunFor \"$secs\"; quit" >"$log" 2>&1

grep -E "LOCKED|non existing" "$log" | sed -E 's/^\[[0-9:.]+\] //' >&2 || true
# Renode folds repeats as "... (N)"; keep them, they're part of the sequence.
grep -E "\] [a-z]+: MMIO W " "$log" | sed -E 's/^.*MMIO W //' | grep -E "$filter" || true
