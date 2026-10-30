# Flashing the RMK firmware

How to put `akko-rmk` on the keyboard, check it, and go back to QMK. The keyboard can't be opened
(no SWD, no BOOT0), so read [Flashing safety](README.md#flashing-safety) first.

## 1. One-time setup

**The flasher.** Build `wb32-dfu-updater_cli` from [Westberry's source](https://github.com/WestberryTech/wb32-dfu-updater)
(`cmake -B build . && cmake --build build`) and put it on `PATH`.

**udev rules**, so neither the flasher nor Vial needs root. The bootloader is `342d:dfa0`. The
keyboard is `342d:e4f6`, over USB and through the 2.4G dongle alike:

```fish
echo 'SUBSYSTEMS=="usb", ATTRS{idVendor}=="342d", ATTRS{idProduct}=="dfa0", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/50-wb32-dfu.rules
echo 'KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="342d", ATTRS{idProduct}=="e4f6", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/59-akko-5075.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

**Back up what's on it.** Firmware can't be read off the keyboard. Keep the vendor's `.hex` (or
your QMK `.bin`) to go back to.

## 2. Build, test, freeze

```fish
python3 -I tools/bench.py                 # http://localhost:8475
```

In the workbench, **Run sim tests** builds every image and runs the whole sim-rs suite
(`sim-rs/run.sh`, about 2 min). When every test passes, it freezes the akko-rmk the tests ran
(`sim-rs/target/images/akko-rmk`) into `target/bench/`, stamped with its sha256. Only a frozen
image can be flashed from there. **Cross-check in Renode** runs the same suites in Renode
(`sim/run.sh`, about 25 min) and only reports; it's worth a run when a change touches timing.

## 3. Enter the bootloader

Any one of these:
- **Fn + Right Shift + Esc** on RMK (QK_BOOT). It goes straight to the ROM bootloader on the press
  and keeps the settings. On the vendor's QMK the same keys also erase its settings.
- **Esc held at a cold start.** The back switch goes to USB with no cable in, then you plug the
  cable in while holding Esc. On battery, plugging in doesn't restart the keyboard, so it has to be
  off first.
- **Vial's bootloader button** on RMK.

The keyboard goes dark. `wb32-dfu-updater_cli -l` should then print `Found DFU`.

A crash also ends up here. A panic resets the keyboard, and a second panic within 60 s of starting
goes to DFU. Vial channel 0xD2 returns the last panic message.

## 4. Flash

- **Workbench:** "Flash RMK" flashes the frozen image. It waits for the bootloader and restarts
  the keyboard afterwards.
- **By hand:**
  ```fish
  python3 -I tools/precheck.py target/bench/akko-rmk.elf target/bench/akko-rmk.bin
  wb32-dfu-updater_cli -D target/bench/akko-rmk.bin
  wb32-dfu-updater_cli -R
  ```

`tools/flash.sh <bin>` builds, prechecks and flashes in one go. It's meant for the bring-up
images; for `akko-rmk`, freeze and flash the tested copy instead.

**The keymap after a flash.** Vial keymap edits survive a flash while RMK's storage schema hash
matches. That hash covers rmk's version and features, the macro, combo and morse sizes in
`firmware/rmk.toml`, and `RMK_COMMIT` in `.cargo/config.toml`. Bump `RMK_COMMIT` only when a change
to our rmk fork alters what's stored. An image with a new hash resets the keymap to the default once,
on its first start. The lighting and keyboard settings live in their own record and are kept.

## 5. Check it works

The workbench's sign-off checklist (`tools/signoff.json`) has these checks and more. It records each
result against the image last flashed from the bench.

- **Typing**, over USB and over 2.4G, including the Win/Mac switch.
- **Lighting:** Fn+L toggles it, and Fn+Del steps the mode. The side lights follow.
- **Knob:** clockwise turns the volume up. A fast spin shouldn't lose steps, over 2.4G either.
- **Bluetooth:** hold Fn+E for 3 s to pair BT1 ("Akko 5075-1"). Fn+R and Fn+T are BT2 and BT3.
- **2.4G and USB:** Fn+Y switches to 2.4G (hold it 3 s to re-pair the dongle). Fn+U goes back to
  USB.
- **Battery:** Fn+Space shows the level.
- **Sleep:** on BT or 2.4G the keyboard sleeps after 2 min idle, and a key wakes it.
  `tools/sleep.py` sets the timeout.
- **Vial:** see below.

Other key combos:
- Fn+Grave, held 3 s: factory reset of the keymap and the lighting. It keeps the selected device
  and restarts the keyboard on release.
- Left Ctrl held at power-up: LED test (red, green, blue, white).
- Esc held at a cold start: bootloader (section 3).

## Vial

The keyboard isn't in Vial's online list, but it serves its own definition, so nothing needs
loading.
- **Over USB:** desktop Vial or [vial.rocks](https://vial.rocks) in Chrome or Chromium (Firefox
  has no WebHID).
- **Through the 2.4G dongle:** vial.rocks works. Desktop Vial only finds the keyboard over USB.

Diagnostics live on extra Vial channels. The scripts in `tools/` use them over either path:

| Channel | What | Tool |
|---|---|---|
| 0xD1 | scan counters and the keys read | workbench Capture |
| 0xD2 | last panic message | workbench Capture |
| 0xD4 | 2.4G/BT link counters | |
| 0xD5 | stack peak | workbench Capture |
| 0xD6 | debounce: eager or defer | `tools/debounce.py` |
| 0xD7 | sleep timeout | `tools/sleep.py` |
| 0xD9 | stop mode while asleep (default: only with the host gone) | `tools/sleep.py stop link\|on\|off` |

## Back to QMK

The bootloader is in ROM, so this always works. Enter it (section 3), then use the workbench's
"Flash QMK", or flash by hand:

```fish
wb32-dfu-updater_cli -D <your QMK or the vendor's .bin>
wb32-dfu-updater_cli -R
```

The vendor's `.hex` converts with `arm-none-eabi-objcopy -I ihex -O binary <hex> <bin>`.
