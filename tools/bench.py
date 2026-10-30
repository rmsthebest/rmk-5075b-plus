#!/usr/bin/env python3
"""Workbench: run the sim tests, flash the tested RMK image, flash QMK back.

    python3 tools/bench.py        (opens http://127.0.0.1:8475 in the browser)
    python3 tools/bench.py test   (the "Run sim tests" button, in the terminal)

Everything is a button, so the keyboard being flashed can be the only one
plugged in. Only the stdlib is used; the page is served on localhost only and
every action needs the token in the URL the script opens.

The sign-off checklist (tools/signoff.json) records pass/fail/skip per
item in target/bench/signoff.json, against the image last flashed from here.

Safety rules, as tools/flash.sh and the sim-first workflow:
  - "Run sim tests" builds every image and runs sim-rs/run.sh, the whole
    suite. Only when every test passes is akko-rmk frozen: the snapshot the
    tests loaded (sim-rs/target/images/akko-rmk), ELF + bin copied to
    target/bench/, with a stamp holding its sha256. "Flash RMK" flashes that
    frozen copy and only after precheck.py passes on it again and its hash
    matches the stamp. A flash copies its image into a directory of its own
    first and checks, prechecks and flashes that copy, so nothing that
    changes target/bench during the wait for DFU reaches the keyboard.
  - "Flash QMK" flashes ../akko_5075b_plus_fixes.bin (a copy, as above) and
    only if its hash is the known-good one below.
  - A flash that fails or is cut off leaves a marker (target/bench/
    flash-incomplete) until a flash succeeds: "Leave DFU" is refused
    meanwhile, as the half-written image could boot erased vectors.
  - The tests run without AKKO_SIM_IMAGES, and run.sh prints the sha256 of
    the akko-rmk it snapshotted; the frozen copy must match it, so an image
    another run swapped in isn't frozen.
  - DFU: "Enter DFU", and flashing, send a running akko-rmk there over USB
    (VIA's bootloader jump, as Vial's button). Anything else (QMK, or an
    image that doesn't answer) is put there by hand (unplug, hold Esc, plug
    in), and flashing waits for it.
"""

import hashlib
import json
import os
import re
import secrets
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "target/bench"
RMK_ELF = ROOT / "target/thumbv7m-none-eabi/release/akko-rmk"
# sim-rs/run.sh's build, and the snapshot of it its tests load.
SIM_BUILD = ROOT / "target/sim-rs/thumbv7m-none-eabi/release/akko-rmk"
TESTED_ELF = ROOT / "sim-rs/target/images/akko-rmk"
FROZEN_ELF = BENCH / "akko-rmk.elf"
FROZEN_BIN = BENCH / "akko-rmk.bin"
STAMP = BENCH / "akko-rmk.json"
# The image last flashed from here, and the sign-off checklist's results.
FLASHED = BENCH / "flashed.json"
# Written before a DFU download, removed once one succeeds.
FLASH_INCOMPLETE = BENCH / "flash-incomplete"
CHECKS = Path(__file__).with_name("signoff.json")
RESULTS_FILE = BENCH / "signoff.json"
LOGS = BENCH / "logs"
QMK_BIN = ROOT.parent / "akko_5075b_plus_fixes.bin"
QMK_SHA = "d5e10b44ed718de45f0da4c4e08914578baa9104895e88a2c3e67e912c3af306"
DFU = "wb32-dfu-updater_cli"
PORT = 8475
DFU_WAIT_S = 180

TOKEN = secrets.token_urlsafe(16)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def usb_devices():
    """(vid, pid, product, serial) of every attached USB device, from sysfs."""
    out = []
    for d in Path("/sys/bus/usb/devices").iterdir():
        try:
            vid = (d / "idVendor").read_text().strip()
            pid = (d / "idProduct").read_text().strip()
        except OSError:
            continue
        def attr(name):
            try:
                return (d / name).read_text().strip()
            except OSError:
                return ""
        out.append((vid, pid, attr("product"), attr("serial")))
    return out


def keyboard_state():
    for vid, pid, product, serial in usb_devices():
        if vid == "342d" and pid == "dfa0":
            return {"mode": "dfu", "text": "DFU bootloader"}
        if vid == "342d" and pid == "e4f6":
            if "RMK" in product:
                return {"mode": "rmk", "text": product, "detail": serial}
            return {"mode": "qmk", "text": product or "QMK"}
    return {"mode": "none", "text": "Not connected"}


def image_state():
    builds = [p for p in (RMK_ELF, SIM_BUILD) if p.exists()]
    st = {"built": bool(builds), "frozen": None}
    if STAMP.exists() and FROZEN_BIN.exists():
        stamp = json.loads(STAMP.read_text())
        stamp["ok"] = sha256(FROZEN_BIN) == stamp["sha256"]
        st["frozen"] = stamp
    if builds and st["frozen"]:
        st["stale"] = max(p.stat().st_mtime for p in builds) > STAMP.stat().st_mtime
    return st


class Job:
    """The one action that may run at a time, with its log."""

    def __init__(self):
        self.lock = threading.Lock()
        self.name = None
        self.status = "idle"  # idle | running | ok | failed | cancelled
        self.log = []
        self.proc = None
        self.cancel = threading.Event()
        self.cancellable = True
        self.prompt = None
        self.echo = False

    def start(self, name, fn):
        with self.lock:
            if self.status == "running":
                return False
            self.name, self.status, self.log = name, "running", []
            self.cancel.clear()
            self.cancellable, self.prompt = True, None
        threading.Thread(target=self._run, args=(fn,), daemon=True).start()
        return True

    def _run(self, fn):
        try:
            fn(self)
            self.status = "ok"
            self.say("Done.")
        except Cancelled:
            self.status = "cancelled"
            self.say("Cancelled.")
        except Exception as e:  # noqa: BLE001 - every failure goes to the log
            self.status = "failed"
            self.say(f"FAILED: {e}")
        finally:
            self.proc, self.prompt = None, None
            self._save()

    def _save(self):
        # Keeps every job's log on disk; the page only shows the latest one.
        try:
            LOGS.mkdir(parents=True, exist_ok=True)
            name = f"{time.strftime('%Y%m%d-%H%M%S')}-{self.name}-{self.status}.log"
            (LOGS / name).write_text("\n".join(self.log) + "\n")
        except OSError as e:
            print(f"can't save the log: {e}", flush=True)

    def say(self, line):
        self.log.append(line)
        if self.echo:
            print(line, flush=True)

    def run(self, *cmd, cwd=ROOT, env=None):
        if self.cancel.is_set():
            raise Cancelled
        self.say("$ " + " ".join(str(c) for c in cmd))
        self.proc = subprocess.Popen(
            [str(c) for c in cmd], cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, errors="replace", start_new_session=True, env=env)
        for line in self.proc.stdout:
            self.say(line.rstrip("\n"))
        code = self.proc.wait()
        if self.cancel.is_set():
            raise Cancelled
        if code != 0:
            raise RuntimeError(f"{cmd[0]} exited with {code}")

    def stop(self):
        if self.status != "running" or not self.cancellable:
            return False
        self.cancel.set()
        if self.proc:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        return True


class Cancelled(Exception):
    pass


JOB = Job()


def rmk_to_dfu(job):
    """Sends a running akko-rmk to the DFU bootloader with VIA's bootloader
    jump (0x0B), as Vial's button does; waits up to 5 s. False if akko-rmk
    isn't connected over USB or didn't go."""
    path = rmk_raw_hid()
    if not path:
        return False
    job.say("Sending akko-rmk to the DFU bootloader (VIA bootloader jump)...")
    fd = os.open(path, os.O_RDWR)
    try:
        os.write(fd, bytes([0x00, 0x0B]) + bytes(31))
    except OSError:
        pass  # it may leave the bus before the write returns
    finally:
        os.close(fd)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if keyboard_state()["mode"] == "dfu":
            return True
        time.sleep(0.25)
    return False


def wait_for_dfu(job):
    if keyboard_state()["mode"] != "dfu" and rmk_to_dfu(job):
        job.say("In the DFU bootloader.")
    if keyboard_state()["mode"] == "dfu":
        time.sleep(0.5)
        return
    job.prompt = ("Unplug the keyboard and set the back switch to USB (on battery, plugging in "
                  "doesn't restart it), hold Esc, plug it back in (keep Esc held for a second).")
    job.say("Waiting for the DFU bootloader...")
    deadline = time.monotonic() + DFU_WAIT_S
    while keyboard_state()["mode"] != "dfu":
        if job.cancel.is_set():
            raise Cancelled
        if time.monotonic() > deadline:
            raise RuntimeError(f"no DFU device after {DFU_WAIT_S} s")
        time.sleep(0.25)
    job.prompt = None
    job.say("Found the DFU bootloader. You can let go of Esc.")
    time.sleep(0.5)


def private_copy(*paths):
    """Copies of `paths` in a directory of their own (the caller removes it)."""
    BENCH.mkdir(parents=True, exist_ok=True)
    d = Path(tempfile.mkdtemp(prefix="flash-", dir=BENCH))
    copies = []
    for p in paths:
        shutil.copy2(p, d / p.name)
        copies.append(d / p.name)
    return d, copies


def flash(job, bin_path):
    wait_for_dfu(job)
    job.cancellable = False
    job.say(f"Flashing {bin_path.name} ({bin_path.stat().st_size} bytes), don't unplug.")
    FLASH_INCOMPLETE.write_text(time.strftime("%Y-%m-%d %H:%M") + f" {bin_path.name}\n")
    job.run(DFU, "-D", bin_path)
    FLASH_INCOMPLETE.unlink()
    job.run(DFU, "-R")
    job.cancellable = True
    job.say("Waiting for the keyboard to come back...")
    for _ in range(40):
        st = keyboard_state()
        if st["mode"] in ("qmk", "rmk"):
            job.say(f"Running: {st['text']}")
            return
        time.sleep(0.25)
    job.say("The keyboard hasn't enumerated yet; check it by hand.")


def cargo_totals(lines):
    """Sums cargo test's "test result: ..." lines."""
    total = {"passed": 0, "failed": 0, "ignored": 0}
    for line in lines:
        if line.startswith("test result:"):
            for n, what in re.findall(r"(\d+) (passed|failed|ignored)", line):
                total[what] += int(n)
    return total


def do_test(job):
    start = len(job.log)
    env = {k: v for k, v in os.environ.items() if k != "AKKO_SIM_IMAGES"}
    try:
        job.run("sim-rs/run.sh", env=env)
    finally:
        total = cargo_totals(job.log[start:])
        job.say(f"Sim: {total['passed']} passed, {total['failed']} failed, {total['ignored']} ignored")
    if total["failed"] or total["ignored"] or not total["passed"]:
        raise RuntimeError("not every test passed, image not frozen")
    tested = [m.group(1) for line in job.log[start:]
              if (m := re.match(r"snapshot akko-rmk ([0-9a-f]{64})$", line))]
    BENCH.mkdir(parents=True, exist_ok=True)
    shutil.copy2(TESTED_ELF, FROZEN_ELF)
    if tested != [sha256(FROZEN_ELF)]:
        raise RuntimeError("the akko-rmk snapshot isn't the one the tests ran (another run.sh?), not frozen")
    job.run("rust-objcopy", "-O", "binary", FROZEN_ELF, FROZEN_BIN)
    job.run(sys.executable, "-I", "tools/precheck.py", FROZEN_ELF, FROZEN_BIN)
    rev = subprocess.run(["git", "describe", "--always", "--dirty"], cwd=ROOT,
                         capture_output=True, text=True).stdout.strip()
    stamp = {"sha256": sha256(FROZEN_BIN), "size": FROZEN_BIN.stat().st_size, "git": rev,
             "tests": total["passed"], "sim": "sim-rs", "time": time.strftime("%Y-%m-%d %H:%M")}
    STAMP.write_text(json.dumps(stamp, indent=1))
    job.say(f"Frozen akko-rmk {stamp['sha256'][:12]} ({rev}), ready to flash.")


def do_flash_rmk(job):
    st = image_state()["frozen"]
    if not st or not st["ok"]:
        raise RuntimeError("no tested image (or it changed since the tests): run the sim tests first")
    d, (elf, binp) = private_copy(FROZEN_ELF, FROZEN_BIN)
    try:
        if sha256(binp) != st["sha256"]:
            raise RuntimeError("the frozen image changed while it was copied: run the sim tests again")
        job.run(sys.executable, "-I", "tools/precheck.py", elf, binp)
        flash(job, binp)
    finally:
        shutil.rmtree(d, ignore_errors=True)
    FLASHED.write_text(json.dumps({"sha256": st["sha256"], "git": st["git"], "time": time.strftime("%Y-%m-%d %H:%M")}))


def signoff_state():
    """The checklist (tools/signoff.json) with each item's last result, and
    the image results are recorded against: the one last flashed from here
    (or the frozen one, if nothing was)."""
    results = json.loads(RESULTS_FILE.read_text()) if RESULTS_FILE.exists() else {}
    image = json.loads(FLASHED.read_text()) if FLASHED.exists() else None
    if image is None and STAMP.exists():
        image = json.loads(STAMP.read_text())
    return {"groups": json.loads(CHECKS.read_text()), "results": results,
            "image": image and image["sha256"][:12], "flashed": FLASHED.exists()}


def record_check(item, result, note):
    ids = {i["id"] for g in json.loads(CHECKS.read_text()) for i in g["items"]}
    if item not in ids or result not in ("pass", "fail", "skip", "clear"):
        return False
    results = json.loads(RESULTS_FILE.read_text()) if RESULTS_FILE.exists() else {}
    if result == "clear":
        results.pop(item, None)
    else:
        results[item] = {"result": result, "image": signoff_state()["image"],
                         "time": time.strftime("%Y-%m-%d %H:%M"), "note": note[:200]}
    BENCH.mkdir(parents=True, exist_ok=True)
    RESULTS_FILE.write_text(json.dumps(results, indent=1))
    return True


def do_flash_qmk(job):
    d, (binp,) = private_copy(QMK_BIN)
    try:
        if sha256(binp) != QMK_SHA:
            raise RuntimeError(f"{QMK_BIN} is not the known-good QMK image")
        flash(job, binp)
    finally:
        shutil.rmtree(d, ignore_errors=True)


def keyboard_nodes():
    """[(label, /dev path, kind)] for the keyboard's hidraw and evdev nodes."""
    nodes = []
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        dev = h.resolve() / "device"
        if "0003:0000342D:0000E4F6" not in (dev / "uevent").read_text().upper():
            continue
        iface = (dev / "..").resolve().name  # e.g. 3-2.3:1.0
        nodes.append((f"if{iface.rsplit('.', 1)[-1]} {h.name}", f"/dev/{h.name}", "hidraw"))
        for ev in sorted((dev / "input").glob("input*/event*")):
            name = (ev.parent / "name").read_text().strip()
            nodes.append((f"if{iface.rsplit('.', 1)[-1]} {ev.name} ({name})", f"/dev/input/{ev.name}", "evdev"))
    return nodes


# VIA id_custom_get_value on the channel akko-rmk's scan answers (src/scan.rs).
DIAG_REQUEST = bytes([0x00, 0x08, 0xD1]) + bytes(30)  # report ID 0, then 32 bytes
DIAG_EVERY_S = 0.1
PANIC_CHANNEL = 0xD2
STACK_CHANNEL = 0xD5


def read_panic_message(fd):
    """The message of the panic that ended akko-rmk's previous run, '' if none,
    None if the keyboard doesn't answer (QMK, or an older image)."""
    msg, off, total = b"", 0, None
    while total is None or off < total:
        os.write(fd, bytes([0x00, 0x08, PANIC_CHANNEL, off]) + bytes(29))
        deadline = time.monotonic() + 0.5
        while True:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([fd], [], [], left)[0]:
                return None
            data = os.read(fd, 64)
            if data[:3] == bytes([0x08, PANIC_CHANNEL, off]):
                break
        total = data[3]
        part = data[4:4 + min(28, total - off)]
        if not part:
            break
        msg, off = msg + part, off + len(part)
    return msg.decode(errors="replace")


def read_stack_peak(fd):
    """(deepest stack use since start-up, stack size) in bytes, or None if the
    image doesn't answer (QMK, or older than channel 0xD5)."""
    os.write(fd, bytes([0x00, 0x08, STACK_CHANNEL]) + bytes(30))
    deadline = time.monotonic() + 0.5
    while True:
        left = deadline - time.monotonic()
        if left <= 0 or not select.select([fd], [], [], left)[0]:
            return None
        data = os.read(fd, 64)
        if data[:2] == bytes([0x08, STACK_CHANNEL]):
            used, size = struct.unpack_from("<II", data, 2)
            return (used, size) if size else None


def say_stack(job, fd, when):
    try:
        peak = read_stack_peak(fd)
    except OSError as e:
        job.say(f"can't ask for the stack peak: {e}")
        return
    if peak:
        used, size = peak
        job.say(f"stack {when}: deepest {used} of {size} B ({size - used} B never used)")


class Diag:
    """Decodes the scan's diagnostics; logs keys seen and, every 2 s, the counters."""

    def __init__(self, job):
        self.job, self.prev, self.last_summary = job, None, 0.0

    def reply(self, ts, data):
        passes, down, flips, events = struct.unpack_from("<IIII", data, 2)
        gap_ms = struct.unpack_from("<H", data, 18)[0] / 10
        seen = [(r, c) for r in range(6) for c in range(15)
                if (data[20 + r] | data[26 + r] << 8) >> c & 1]
        now = float(ts)
        rate = ""
        if self.prev:
            p_passes, p_down, p_flips, p_events, p_t = self.prev
            dt = now - p_t
            if dt > 0:
                rate = f"{(passes - p_passes) / dt:.0f} passes/s"
            new = (down - p_down, flips - p_flips, events - p_events)
        else:
            new = (0, 0, 0)
        if seen or new[1] or new[2]:
            self.job.say(f"{ts} scan: read down {seen}; +{new[0]} passes with a key down, "
                         f"+{new[1]} flips, +{new[2]} events; gap {gap_ms} ms; {rate}")
        elif now - self.last_summary >= 2:
            self.job.say(f"{ts} scan: {passes} passes ({rate}), {down} with a key down, {flips} flips, "
                         f"{events} events; longest gap {gap_ms} ms")
            self.last_summary = now
        self.prev = (passes, down, flips, events, now)


def do_capture(job):
    """Logs what the kernel receives from the keyboard (hidraw) and makes of it (evdev)."""
    nodes = keyboard_nodes()
    if not nodes:
        raise RuntimeError("the keyboard isn't connected")
    raw_hid = set()  # Vial/VIA raw HID (usage page 0xFF60): asked for scan diagnostics
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        if any(path == f"/dev/{h.name}" for _, path, _ in nodes):
            rd = (h.resolve() / "device/report_descriptor").read_bytes()
            job.say(f"{h.name} report descriptor ({len(rd)} B): {rd.hex(' ')}")
            if rd.startswith(b"\x06\x60\xff"):
                raw_hid.add(f"/dev/{h.name}")
    fds = {}
    diag_fd = None
    for label, path, kind in nodes:
        try:
            if path in raw_hid:
                fd = diag_fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
            else:
                fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
            fds[fd] = (label, kind)
            job.say(f"listening: {label}" + (" (scan diagnostics)" if fd == diag_fd else ""))
        except OSError as e:
            job.say(f"can't open {label}: {e}")
    diag = Diag(job)
    if diag_fd is not None:
        try:
            msg = read_panic_message(diag_fd)
        except OSError as e:
            msg = None
            job.say(f"can't ask for the last panic: {e}")
        if msg:
            job.say(f"previous run panicked: {msg}")
        elif msg == "":
            job.say("no panic in the previous run")
        say_stack(job, diag_fd, "at the start")
    job.prompt = ("Slowly, one key at a time: a, s, d. Caps Lock twice. Hold Win, tap e, release Win. "
                  "Hold Fn, tap F6, release Fn. Turn the knob. Watch the Win-lock LED.")
    end = time.monotonic() + 60
    t0 = time.monotonic()
    next_ask = t0
    try:
        while time.monotonic() < end:
            if job.cancel.is_set():
                raise Cancelled
            if diag_fd is not None and time.monotonic() >= next_ask:
                next_ask += DIAG_EVERY_S
                try:
                    os.write(diag_fd, DIAG_REQUEST)
                except OSError as e:
                    job.say(f"scan diagnostics: can't ask: {e}")
                    diag_fd = None
            ready, _, _ = select.select(list(fds), [], [], 0.2)
            for fd in ready:
                label, kind = fds[fd]
                data = os.read(fd, 4096)
                ts = f"{time.monotonic() - t0:6.3f}"
                if kind == "hidraw":
                    if fd == diag_fd and data[:2] == DIAG_REQUEST[1:3]:
                        diag.reply(ts, data)
                    elif fd == diag_fd and data[:1] == b"\xff":
                        pass  # QMK: id_unhandled
                    else:
                        job.say(f"{ts} {label}: {data.hex(' ')}")
                    continue
                for off in range(0, len(data) - 23, 24):
                    _, _, typ, code, val = struct.unpack_from("qqHHi", data, off)
                    if typ == 1:  # EV_KEY
                        job.say(f"{ts} {label}: key {code} {('up', 'down', 'repeat')[val] if val < 3 else val}")
                    elif typ == 17:  # EV_LED
                        job.say(f"{ts} {label}: led {code} {val}")
    finally:
        if diag_fd is not None:
            say_stack(job, diag_fd, "at the end")
        for fd in fds:
            os.close(fd)
        job.prompt = None


def rmk_raw_hid():
    """/dev path of akko-rmk's Vial raw HID node, or None."""
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        dev = h.resolve() / "device"
        uevent = (dev / "uevent").read_text()
        # The 2.4G dongle has the same ids (and a Vial node): only "(RMK)".
        if "0003:0000342D:0000E4F6" not in uevent.upper() or "(RMK)" not in uevent:
            continue
        if (dev / "report_descriptor").read_bytes().startswith(b"\x06\x60\xff"):
            return f"/dev/{h.name}"
    return None


def do_crash(job):
    """One deliberate panic (Vial channel 0xD3): RMK should reset into itself
    and report it. A second within 60 s would go to DFU, so this sends one."""
    provoke(job, 0xD3, "P", "Panic requested", "requested over Vial")


def do_hang(job):
    """One deliberate hang (Vial channel 0xD8): the software watchdog should
    panic within about 2 s, and RMK reset into itself and report it. As a
    crash, a second within 60 s would go to DFU."""
    provoke(job, 0xD8, "H", "Hang requested (the keys stop for about 2 s)", "src/watchdog.rs")


def provoke(job, channel, confirm, said, expect):
    path = rmk_raw_hid()
    if not path:
        raise RuntimeError("akko-rmk isn't connected over USB (cable in, USB mode)")
    fd = os.open(path, os.O_RDWR)
    try:
        os.write(fd, bytes([0x00, 0x08, channel, ord(confirm)]) + bytes(29))
    except OSError:
        pass  # it may reset before the write returns
    finally:
        os.close(fd)
    job.say(f"{said}. Waiting for the keyboard to come back...")
    # A hang keeps the old device on the bus until the watchdog resets it.
    time.sleep(3.0 if channel == 0xD8 else 1.0)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if job.cancel.is_set():
            raise Cancelled
        if any(v == "342d" and p == "dfa0" for v, p, *_ in usb_devices()):
            raise RuntimeError("the keyboard went to DFU instead of restarting (use Leave DFU)")
        path = rmk_raw_hid()
        if path:
            time.sleep(0.5)
            fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
            try:
                msg = read_panic_message(fd)
            finally:
                os.close(fd)
            if msg and expect in msg:
                job.say(f"Back in RMK, which reports: {msg}")
                return
            raise RuntimeError(f"back in RMK, but the panic report was {msg!r}")
        time.sleep(0.2)
    raise RuntimeError("the keyboard didn't come back within 15 s")


def do_enter_dfu(job):
    if keyboard_state()["mode"] == "dfu":
        job.say("Already in the DFU bootloader.")
        return
    if not rmk_to_dfu(job):
        raise RuntimeError("akko-rmk isn't connected over USB (cable in, USB mode), or it didn't go to DFU")
    job.say("In the DFU bootloader. Flash, or Leave DFU to restart.")


def do_reset(job):
    if FLASH_INCOMPLETE.exists():
        raise RuntimeError("the last flash didn't finish (" + FLASH_INCOMPLETE.read_text().strip()
                           + "): flash again; leaving DFU could boot a half-written image")
    job.run(DFU, "-R")


ACTIONS = {
    "test": ("Run sim tests", do_test),
    "flash-rmk": ("Flash RMK", do_flash_rmk),
    "flash-qmk": ("Flash QMK", do_flash_qmk),
    "capture": ("Capture 60 s", do_capture),
    "crash": ("Test crash recovery", do_crash),
    "hang": ("Test watchdog", do_hang),
    "dfu": ("Enter DFU", do_enter_dfu),
    "reset": ("Leave DFU", do_reset),
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, code, body, ctype="application/json"):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def authed(self):
        return secrets.compare_digest(self.headers.get("X-Token", ""), TOKEN)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/":
            if parse_qs(url.query).get("t", [""])[0] != TOKEN:
                return self.send(403, "Open the URL tools/bench.py printed.", "text/plain")
            return self.send(200, PAGE, "text/html; charset=utf-8")
        if url.path == "/api/state" and self.authed():
            since = int(parse_qs(url.query).get("since", ["0"])[0])
            return self.send(200, json.dumps({
                "keyboard": keyboard_state(), "image": image_state(),
                "flash_incomplete": FLASH_INCOMPLETE.exists(),
                "qmk": {"present": QMK_BIN.exists(), "path": str(QMK_BIN)},
                "signoff": signoff_state(),
                "job": {"name": JOB.name, "status": JOB.status, "prompt": JOB.prompt,
                        "cancellable": JOB.cancellable, "log": JOB.log[since:], "len": len(JOB.log)},
            }))
        self.send(404, "{}")

    def do_POST(self):
        if not self.authed():
            return self.send(403, "{}")
        what = urlparse(self.path).path.removeprefix("/api/")
        if what == "cancel":
            return self.send(200, json.dumps({"ok": JOB.stop()}))
        if what == "check":
            n = min(int(self.headers.get("Content-Length", "0")), 4096)
            try:
                body = json.loads(self.rfile.read(n) or b"{}")
                ok = record_check(str(body.get("id", "")), str(body.get("result", "")), str(body.get("note", "")))
            except ValueError:
                ok = False
            return self.send(200 if ok else 400, json.dumps({"ok": ok}))
        if what in ACTIONS:
            label, fn = ACTIONS[what]
            return self.send(200, json.dumps({"ok": JOB.start(label, fn)}))
        self.send(404, "{}")


PAGE = (Path(__file__).with_name("bench.html")).read_text()


def main():
    if sys.argv[1:2] == ["test"]:
        JOB.echo = True
        JOB._run(do_test)
        sys.exit(0 if JOB.status == "ok" else 1)
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    url = f"http://127.0.0.1:{PORT}/?t={TOKEN}"
    print(f"Workbench at {url}  (Ctrl+C to stop)", flush=True)
    if "--no-browser" not in sys.argv:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
