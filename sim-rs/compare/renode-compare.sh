#!/usr/bin/env bash
# Traces <image> in Renode and runs compare-renode on it, again while the
# comparison finds time-dependent reads that need Renode's registers.
#   sim-rs/compare/renode-compare.sh <image> [secs=0.02]
set -uo pipefail
cd "$(dirname "$0")/.."
for _ in 1 2 3 4 5 6 7 8; do
    compare/renode-trace.sh "$@" || exit 1
    cargo run --offline --release -q --bin compare-renode -- "$1"
    status=$?
    [ $status = 2 ] || exit $status
done
echo "still finding new reads after 8 runs" >&2
exit 1
