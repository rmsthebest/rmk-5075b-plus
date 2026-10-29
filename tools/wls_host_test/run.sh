#!/bin/sh
# Host tests for keyboards/linker/wireless/{module,smsg}.c under ASan/UBSan.
#   test_parser_queue: UART frame parser (resync, bounds, checksum) and send queue (ACK, retry limit)
#   dump_wire:         every md_send_* / md_devs_change packet, compared with wire_expected.txt
#   dump_board:        md_devs_change with the board's strings and ARM's -fshort-wchar, compared with wire_board.txt
# Both files live in crates/qmk-wireless/tests/, where the Rust port is checked against them.
# Needs the QMK tree with the wireless fixes (branch akko-5075b-plus): QMK_TREE, by default
# ../qmk_5075b_plus next to this repo.
# Regenerate the files only for an intended protocol change:  ./run.sh --update
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
DATA=$REPO/crates/qmk-wireless/tests
W=${QMK_TREE:-$REPO/../qmk_5075b_plus}/keyboards/linker/wireless
[ -f "$W/module.c" ] || { echo "no $W/module.c: set QMK_TREE to the QMK tree" >&2; exit 2; }
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
CFLAGS="-std=gnu11 -Wall -Wextra -Wno-unused-parameter -fsanitize=address,undefined -fno-sanitize-recover=all -g -I$HERE/stubs -I$W"

cc $CFLAGS "$HERE/test_parser_queue.c" "$W/module.c" "$W/smsg.c" -o "$OUT/test"
"$OUT/test"

cc $CFLAGS "$HERE/dump_wire.c" "$W/module.c" "$W/smsg.c" -o "$OUT/dump"
"$OUT/dump" > "$OUT/wire.txt"
cc -I"$HERE/board_stubs" $CFLAGS -fshort-wchar "$HERE/dump_board.c" "$W/module.c" "$W/smsg.c" -o "$OUT/board"
"$OUT/board" > "$OUT/board.txt"
for f in wire:wire_expected board:wire_board; do
    out=$OUT/${f%%:*}.txt want=$DATA/${f#*:}.txt
    if [ "${1:-}" = "--update" ]; then
        cp "$out" "$want"
        echo "$(basename "$want") updated"
    elif diff -u "$want" "$out"; then
        echo "$(basename "$want"): bytes match ($(wc -l < "$want") packets)"
    else
        echo "$(basename "$want"): bytes changed" >&2
        exit 1
    fi
done
