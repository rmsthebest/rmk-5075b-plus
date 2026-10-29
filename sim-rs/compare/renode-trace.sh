#!/usr/bin/env bash
# Records a Renode run for compare-renode (step 3 of the plan, while both
# simulators exist): every executed instruction (PC and opcode) and every
# access to the chip's peripherals, with values.
#   sim-rs/compare/renode-trace.sh <image> [secs=0.02]
# <image> is a file in sim-rs/target/images (sim-rs/run.sh makes them), or
# "rom": the ROM bootloader from $ROM_WORK/dump, entered as boot-guard does
# (rom/sim/rom.resc).
# Writes sim-rs/target/renode/<image>.trace and .log. PCs listed in
# <image>.hooks (compare-renode writes it) get a register snapshot in the log
# each time they run.
set -euo pipefail
cd "$(dirname "$0")/../.."

image=${1:?usage: renode-trace.sh <image> [secs]}
secs=${2:-0.02}
RENODE=${RENODE:-$(cd .. && pwd)/tools/renode}
out=sim-rs/target/renode
mkdir -p "$out"
if [ "$image" = rom ]; then
    dump=${ROM_WORK:-$(cd .. && pwd)/wb32-rom-re}/dump
    [ -f "$dump/rom.bin" ] || { echo "no $dump/rom.bin: see rom/README.md" >&2; exit 1; }
    # rom.resc quiets gpio to ERROR, which hides its access log too.
    setup="\$dump=@$dump; include @rom/sim/rom.resc; runMacro \$enter_rom; logLevel 1 gpio;"
    peripherals="gpio sysctl cache usb sys fmc"
else
    elf=$PWD/sim-rs/target/images/$image
    [ -f "$elf" ] || { echo "no $elf: run sim-rs/run.sh" >&2; exit 1; }
    setup="include @sim/akko.resc; sysbus LoadELF @$elf; machine Reset;"
    peripherals="gpio sysctl cache usb qspi spim2 uart3 tim2"
fi

log=""
for p in $peripherals; do
    log="$log sysbus LogPeripheralAccess $p true;"
done
hooks=$out/$image.hooks
if [ -f "$hooks" ]; then
    for pc in $(cat "$hooks"); do
        log="$log cpu AddHook $pc \"self.InfoLog('snap 0x%x %s' % (self.PC.RawValue, ' '.join(hex(self.GetRegisterUnsafe(i).RawValue) for i in range(13))))\";"
    done
fi
# The log goes to its own file, apart from the monitor's output; Renode
# rotates an existing trace to .trace.N, so old ones go first.
rm -f "$out/$image.log"
rm -f "$out/$image".trace*
"$RENODE/renode" --console --disable-gui --plain -e "logFile @$PWD/$out/$image.log; $setup \
    cpu CreateExecutionTracing \"trace\" @$PWD/$out/$image.trace PCAndOpcode; \
    $log logLevel 3 flash; emulation RunFor \"$secs\"; quit" >"$out/$image.console" 2>&1
echo "$out/$image.trace: $(wc -l <"$out/$image.trace") instructions"
