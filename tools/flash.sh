#!/usr/bin/env bash
# Build one firmware image, precheck it, and flash it over the ROM DFU bootloader.
#   tools/flash.sh <bin-name> [cargo args...]
# e.g. tools/flash.sh m1a-bounce --features always-dfu
# The keyboard must be in DFU (wb32-dfu-updater_cli -l shows "Found DFU"), or
# running an image with the serial console, which is sent the dfu command.
set -euo pipefail
cd "$(dirname "$0")/.."

name=${1:?usage: tools/flash.sh <bin-name> [cargo args...]}; shift
cargo build --release --bin "$name" "$@"
elf=target/thumbv7m-none-eabi/release/$name
bin=target/thumbv7m-none-eabi/release/$name.bin
rust-objcopy -O binary "$elf" "$bin"

python3 -I tools/precheck.py "$elf" "$bin"

in_dfu() { wb32-dfu-updater_cli -l 2>&1 | grep -q "Found DFU"; }

# A running m3-usb image (or later ones with the same serial console) goes to
# DFU on the "dfu" command, so no key combo is needed while developing.
if ! in_dfu; then
    for port in /dev/serial/by-id/usb-akko-rmk_*-if00; do
        [ -e "$port" ] || continue
        echo "Sending 'dfu' to $port"
        printf '\rdfu\r' > "$port" || true
        for _ in $(seq 50); do in_dfu && break; sleep 0.2; done
    done
fi
if ! in_dfu; then
    echo "No DFU device. Enter the bootloader first (Fn + Right Shift + Esc, or Esc held at a cold start)." >&2
    exit 1
fi

echo "Flashing $bin ($(stat -c %s "$bin") bytes)"
wb32-dfu-updater_cli -D "$bin"
wb32-dfu-updater_cli -R
