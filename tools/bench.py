#!/usr/bin/env python3
"""Workbench: run the sim tests, flash the tested RMK image, flash QMK back.

    python3 tools/bench.py        (opens http://127.0.0.1:8475 in the browser)

Everything is a button, so the keyboard being flashed can be the only one
plugged in. Only the stdlib is used; the page is served on localhost only and
every action needs the token in the URL the script opens.

Safety rules, as tools/flash.sh and the sim-first workflow:
  - "Run sim tests" builds every image and runs sim/run.sh. Only when all
    suites pass is akko-rmk frozen (ELF + bin copied to target/bench/) with a
    stamp holding its sha256. "Flash RMK" flashes that frozen copy and only
    after precheck.py passes on it again and its hash matches the stamp.
  - "Flash QMK" flashes ../akko_5075b_plus_fixes.bin and only if its hash is
    the known-good one below.
  - Nothing enters DFU from here: the keyboard is put there by hand (unplug,
    hold Esc, plug in), and flashing waits for it.
"""

import hashlib
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import threading
import time
import webbrowser
import xml.etree.ElementTree as ET
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "target/bench"
RMK_ELF = ROOT / "target/thumbv7m-none-eabi/release/akko-rmk"
FROZEN_ELF = BENCH / "akko-rmk.elf"
FROZEN_BIN = BENCH / "akko-rmk.bin"
STAMP = BENCH / "akko-rmk.json"
QMK_BIN = ROOT.parent / "akko_5075b_plus_fixes.bin"
QMK_SHA = "d5e10b44ed718de45f0da4c4e08914578baa9104895e88a2c3e67e912c3af306"
RESULTS = ROOT / "target/sim-results/robot_output.xml"
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
    st = {"built": RMK_ELF.exists(), "frozen": None}
    if STAMP.exists() and FROZEN_BIN.exists():
        stamp = json.loads(STAMP.read_text())
        stamp["ok"] = sha256(FROZEN_BIN) == stamp["sha256"]
        st["frozen"] = stamp
    if RMK_ELF.exists() and st["frozen"]:
        st["stale"] = RMK_ELF.stat().st_mtime > STAMP.stat().st_mtime
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

    def say(self, line):
        self.log.append(line)

    def run(self, *cmd, cwd=ROOT):
        if self.cancel.is_set():
            raise Cancelled
        self.say("$ " + " ".join(str(c) for c in cmd))
        self.proc = subprocess.Popen(
            [str(c) for c in cmd], cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, errors="replace", start_new_session=True)
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


def wait_for_dfu(job):
    if keyboard_state()["mode"] == "dfu":
        return
    job.prompt = "Unplug the keyboard, hold Esc, plug it back in (keep Esc held for a second)."
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


def flash(job, bin_path):
    wait_for_dfu(job)
    job.cancellable = False
    job.say(f"Flashing {bin_path.name} ({bin_path.stat().st_size} bytes), don't unplug.")
    job.run(DFU, "-D", bin_path)
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


def do_test(job):
    job.run("sim/run.sh")
    root = ET.parse(RESULTS).getroot()
    total = root.find("statistics/total/stat").attrib
    job.say(f"Sim: {total['pass']} passed, {total['fail']} failed, {total['skip']} skipped")
    if int(total["fail"]) or int(total["skip"]) or not int(total["pass"]):
        raise RuntimeError("not every test passed, image not frozen")
    BENCH.mkdir(parents=True, exist_ok=True)
    shutil.copy2(RMK_ELF, FROZEN_ELF)
    job.run("rust-objcopy", "-O", "binary", FROZEN_ELF, FROZEN_BIN)
    job.run(sys.executable, "-I", "tools/precheck.py", FROZEN_ELF, FROZEN_BIN)
    rev = subprocess.run(["git", "describe", "--always", "--dirty"], cwd=ROOT,
                         capture_output=True, text=True).stdout.strip()
    stamp = {"sha256": sha256(FROZEN_BIN), "size": FROZEN_BIN.stat().st_size, "git": rev,
             "tests": int(total["pass"]), "time": time.strftime("%Y-%m-%d %H:%M")}
    STAMP.write_text(json.dumps(stamp, indent=1))
    job.say(f"Frozen akko-rmk {stamp['sha256'][:12]} ({rev}), ready to flash.")


def do_flash_rmk(job):
    st = image_state()["frozen"]
    if not st or not st["ok"]:
        raise RuntimeError("no tested image (or it changed since the tests): run the sim tests first")
    job.run(sys.executable, "-I", "tools/precheck.py", FROZEN_ELF, FROZEN_BIN)
    flash(job, FROZEN_BIN)


def do_flash_qmk(job):
    if sha256(QMK_BIN) != QMK_SHA:
        raise RuntimeError(f"{QMK_BIN} is not the known-good QMK image")
    flash(job, QMK_BIN)


def do_reset(job):
    job.run(DFU, "-R")


ACTIONS = {
    "test": ("Run sim tests", do_test),
    "flash-rmk": ("Flash RMK", do_flash_rmk),
    "flash-qmk": ("Flash QMK", do_flash_qmk),
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
                "qmk": {"present": QMK_BIN.exists(), "path": str(QMK_BIN)},
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
        if what in ACTIONS:
            label, fn = ACTIONS[what]
            return self.send(200, json.dumps({"ok": JOB.start(label, fn)}))
        self.send(404, "{}")


PAGE = (Path(__file__).with_name("bench.html")).read_text()


def main():
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
