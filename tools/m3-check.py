#!/usr/bin/env python3
"""Hardware check for the m3-usb image, step by step.

Run it after `tools/flash.sh m3-usb`, from a terminal that keeps focus (the
keyboard types into it). It checks what the simulator can't: real
enumeration, the CDC log, typing, the Caps Lock LED, suspend/resume, and that
a panic's message survives the trip through the ROM bootloader. It ends with
the keyboard in DFU, ready for the QMK build to go back on.

    python3 -I tools/m3-check.py

The serial port and usbhid-dump need udev rules (once), as for the DFU device:

    printf '%s\n' 'SUBSYSTEM=="tty", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", TAG+="uaccess"' 'SUBSYSTEM=="usb", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/59-akko-rmk-m3.rules
    sudo udevadm control --reload-rules
"""

import glob
import os
import select
import subprocess
import sys
import termios
import time

VID_PID = "1209:0001"
PORT_GLOB = "/dev/serial/by-id/usb-akko-rmk_5075B_Plus_M3_USB_test_m3-if00"


def wait_for(what, check, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        r = check()
        if r:
            return r
        time.sleep(0.2)
    sys.exit(f"FAIL: {what} not seen within {timeout} s")


def port_path():
    found = glob.glob(PORT_GLOB)
    return found[0] if found else None


def dfu_present():
    out = subprocess.run(["wb32-dfu-updater_cli", "-l"], capture_output=True, text=True)
    return "Found DFU" in out.stdout + out.stderr


class Port:
    def __init__(self, path):
        self.fd = os.open(path, os.O_RDWR | os.O_NOCTTY)
        attrs = termios.tcgetattr(self.fd)
        attrs[0] = attrs[1] = attrs[3] = 0  # raw: no input/output/local processing
        attrs[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
        termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
        self.buf = b""

    def read_for(self, seconds):
        """Prints and collects log lines for `seconds`."""
        lines = []
        deadline = time.monotonic() + seconds
        while (left := deadline - time.monotonic()) > 0:
            if select.select([self.fd], [], [], left)[0]:
                try:
                    self.buf += os.read(self.fd, 256)
                except OSError:
                    break  # the device went away
                while b"\n" in self.buf:
                    line, self.buf = self.buf.split(b"\n", 1)
                    text = line.decode(errors="replace").rstrip("\r")
                    print("  | " + text)
                    lines.append(text)
        return lines

    def send(self, cmd):
        # The leading CR ends any partial line left in the keyboard's buffer.
        os.write(self.fd, b"\r" + cmd.encode() + b"\r")

    def close(self):
        os.close(self.fd)


def step(title):
    print(f"\n== {title}")


def ask(question):
    return input(f"{question} [y/n] ").strip().lower().startswith("y")


def main():
    failures = []

    step("Enumeration")
    wait_for("USB device " + VID_PID, lambda: subprocess.run(
        ["lsusb", "-d", VID_PID], capture_output=True).returncode == 0, 10)
    print(subprocess.run(["lsusb", "-d", VID_PID], capture_output=True, text=True).stdout.strip())
    path = wait_for("serial port", port_path, 10)

    step("Log since boot (and any panic left by the previous run)")
    port = Port(path)
    lines = port.read_for(1.0)
    if not any("m3-usb: boot" in l for l in lines):
        failures.append("no boot banner in the log")
    port.send("info")
    if not any("configured true" in l for l in port.read_for(0.5)):
        failures.append("info: not configured")

    step("Typing")
    print("The keyboard types 'm3 ok' and Enter into this terminal:")
    port.send("type")
    typed = input("> ")
    port.read_for(0.3)
    if typed.strip() != "m3 ok":
        failures.append(f"typed {typed!r}, expected 'm3 ok'")

    step("Caps Lock LED")
    print("Press Caps Lock on another keyboard (twice, to turn it back off).")
    port.read_for(6.0)
    if not ask("Did the Akko's Caps Lock LED follow it?"):
        failures.append("Caps Lock LED")

    step("Win Lock LED = configured and not suspended")
    if not ask("Is the Win Lock LED on now?"):
        failures.append("Win Lock LED while configured")
    print("Optional: suspend the computer, wake it, then come back here.")
    if ask("Did you suspend and resume?"):
        lines = port.read_for(1.0)
        port.send("info")
        lines += port.read_for(0.5)
        if not (any("usb: suspended" in l for l in lines) and any("usb: resumed" in l for l in lines)):
            # The port is new after a resume if the host re-enumerated.
            print("  (no suspend/resume lines; the host may have reset the device instead)")
        if not ask("Was the Win Lock LED off while suspended and on again after?"):
            failures.append("Win Lock LED across suspend")

    # After typing: usbhid-dump unbinds and rebinds the kernel driver, and the
    # desktop takes a moment to pick up the new input device.
    step("Report descriptor (usbhid-dump)")
    subprocess.run(["usbhid-dump", "-d", VID_PID, "-e", "descriptor"])
    port.read_for(0.5)
    print("Any 'cdc: port opened/closed' line not caused by this script means")
    print("another program is reading the port.")

    step("A panic lands in DFU, and its message survives to the next boot")
    port.send("panic")
    port.close()
    wait_for("DFU device after the panic", dfu_present, 10)
    print("In DFU. Resetting back into m3-usb.")
    subprocess.run(["wb32-dfu-updater_cli", "-R"])
    path = wait_for("serial port after the reset", port_path, 10)
    time.sleep(0.3)
    port = Port(path)
    lines = port.read_for(1.0)
    if any("previous run panicked: " in l and "requested over USB" in l for l in lines):
        print("  panic message survived the ROM bootloader")
    elif any("no panic record" in l for l in lines):
        failures.append("panic record lost: the ROM bootloader may use RAM above 28K")
    else:
        failures.append("no panic report line after the reset")

    step("Back to DFU")
    port.send("dfu")
    port.close()
    wait_for("DFU device", dfu_present, 10)
    print("In DFU. Put QMK back with:\n  wb32-dfu-updater_cli -D ~/stuff/akko/akko_5075b_plus_fixes.bin && wb32-dfu-updater_cli -R")

    print()
    if failures:
        print("FAILED: " + "; ".join(failures))
        sys.exit(1)
    print("m3-usb hardware check passed")


if __name__ == "__main__":
    main()
