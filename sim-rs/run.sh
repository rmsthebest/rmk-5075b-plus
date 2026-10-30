#!/usr/bin/env bash
# Build the firmware images, snapshot them, and run the simulator's tests.
#   sim-rs/run.sh                 everything
#   sim-rs/run.sh <cargo test args>   e.g. sim-rs/run.sh -p akko-sim boot
# The gate for every image (tools/bench.py's "Run sim tests"): the whole
# suite, about 100 s. --no-fail-fast, so a run reports every failure.
# The images are built in target/sim-rs* rather than the repo's own target/,
# so other builds don't disturb them. They are then copied to sim-rs/target/images/, which
# is all the tests read: a cargo build during a run can't swap an image under
# them. AKKO_SIM_IMAGES= points the tests at another snapshot. The ROM
# suites also read the ROM dump from $ROM_WORK/dump (../rom/README.md).
set -euo pipefail
cd "$(dirname "$0")/.."

build=target/sim-rs
# m1a needs boot-guard's always-dfu feature, so it's built on its own.
cargo build --release --target-dir "$build" --bin m1b-stub
cargo build --release --target-dir "$build" --bin m1a-bounce --features always-dfu
cargo build --release --target-dir "$build" --bin m2-blink
cargo build --release --target-dir "$build" --bin m3-usb
cargo build --release --target-dir "$build" --bin akko-rmk
cargo build --release --target-dir "$build-mac" --bin akko-rmk --features mac
cargo build --release --target-dir "$build-rom-peek" --bin akko-rmk --features rom-peek
# rom/rust's bootloader, in its own target dir.
(cd rom/rust && cargo build --release)

images=sim-rs/target/images
rm -rf "$images.new" && mkdir -p "$images.new"
rel=thumbv7m-none-eabi/release
for bin in m1a-bounce m1b-stub m2-blink m3-usb akko-rmk; do
    cp "$build/$rel/$bin" "$images.new/$bin"
done
cp "$build-mac/$rel/akko-rmk" "$images.new/akko-rmk-mac"
cp "$build-rom-peek/$rel/akko-rmk" "$images.new/akko-rmk-rom-peek"
cp "rom/rust/target/$rel/wb32-rom-dfu" "$images.new/wb32-rom-dfu"
# The QMK build in daily use, built from the vendor's source outside the repo.
# Its ELF has the symbols the QMK tests need; its bytes must be the .bin's.
qmk=${QMK_ELF:-../qmk_5075b_plus/.build/akko_5075b_plus_via.elf}
qmk_sha=d5e10b44ed718de45f0da4c4e08914578baa9104895e88a2c3e67e912c3af306
[ -f "$qmk" ] || { echo "no QMK ELF at $qmk (set QMK_ELF)" >&2; exit 1; }
arm-none-eabi-objcopy -O binary "$qmk" "$images.new/qmk.bin"
sha=$(sha256sum "$images.new/qmk.bin" | cut -d' ' -f1)
rm "$images.new/qmk.bin"
[ "$sha" = "$qmk_sha" ] || { echo "$qmk isn't the known-good QMK build ($sha)" >&2; exit 1; }
cp "$qmk" "$images.new/qmk-via"
rm -rf "$images" && mv "$images.new" "$images"

cd sim-rs
cargo test --release --no-fail-fast "$@"
