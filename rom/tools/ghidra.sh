#!/usr/bin/env bash
# Imports the ROM into a headless Ghidra project ($ROM_WORK/ghidra-project),
# sets up the memory map, vectors and labels (rom/ghidra/scripts/
# RomSetup.java), runs auto-analysis and writes the exports (RomExport.java)
# to $ROM_WORK/out/. $ROM_WORK defaults to ../wb32-rom-re beside the repo
# (rom/README.md): the exports are decompiled vendor code, kept out of git.
#
# Usage: rom/tools/ghidra.sh [--rom FILE] [--base ADDR] [--size N] [--out DIR]
#   defaults: $ROM_WORK/dump/rom.bin, 0x1FFFE000, 4096, $ROM_WORK/out/
# Re-running replaces the program in the project (manual renames in the GUI
# are lost; keep them in notes.md or in RomSetup's label list).
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(realpath -m "${ROM_WORK:-$here/../../wb32-rom-re}")
rom=$work/dump/rom.bin base=0x1FFFE000 size=4096 out=$work/out
while [ $# -gt 0 ]; do
    case $1 in
        --rom) rom=$(realpath "$2"); shift 2 ;;
        --base) base=$2; shift 2 ;;
        --size) size=$2; shift 2 ;;
        --out) out=$(realpath -m "$2"); shift 2 ;;
        *) sed -n '2,12p' "$0"; exit 2 ;;
    esac
done
[ -f "$work/out/labels.csv" ] || { echo "run rom/tools/gen_labels.py first" >&2; exit 1; }
headless=${GHIDRA_HEADLESS:-/opt/ghidra/support/analyzeHeadless}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# The import names the program after the file: always rom.bin.
head -c "$size" "$rom" > "$tmp/rom.bin"
mkdir -p "$work/ghidra-project" "$out"
# Low priority: the sim suite may be running on this machine.
nice -n 19 "$headless" "$work/ghidra-project" rom \
    -import "$tmp/rom.bin" -overwrite \
    -loader BinaryLoader -loader-baseAddr "$base" \
    -processor ARM:LE:32:Cortex -cspec default \
    -scriptPath "$here/ghidra/scripts" \
    -preScript RomSetup.java "$work/out/labels.csv" "$work/out/vectors.csv" "$work/dump/info.bin" \
    -postScript RomExport.java "$out" \
    2>&1 | tee "$out/ghidra.log" | grep -E 'RomSetup|RomExport|ERROR|Exception' || true
grep -q 'RomExport: wrote' "$out/ghidra.log" || { echo "ghidra.sh: failed, see $out/ghidra.log" >&2; exit 1; }
