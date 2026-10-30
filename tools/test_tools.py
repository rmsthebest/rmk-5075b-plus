#!/usr/bin/env python3
"""Negative checks for the flashing tools (retro review 2026-10-30: A-2, A-4,
A-7, A-9). Run from the repo root, after sim-rs/run.sh has built the images:

    python3 -I tools/test_tools.py

precheck.py runs on the sim's akko-rmk snapshot; bench.py's flashing runs
with a stand-in job that records the commands instead of running them.
"""

import hashlib
import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent
ELF = ROOT / "sim-rs/target/images/akko-rmk"


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class Precheck(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.elf = self.dir / "akko-rmk.elf"
        self.bin = self.dir / "akko-rmk.bin"
        shutil.copy2(ELF, self.elf)
        subprocess.run(["rust-objcopy", "-O", "binary", self.elf, self.bin], check=True)

    def tearDown(self):
        shutil.rmtree(self.dir)

    def precheck(self):
        return subprocess.run([sys.executable, "-I", TOOLS / "precheck.py", self.elf, self.bin],
                              capture_output=True, text=True)

    def test_the_elfs_own_bin_isnt_refused_as_another_image(self):
        self.assertNotIn("is not", self.precheck().stdout)

    def test_a_bin_that_isnt_the_elfs_is_refused(self):
        data = bytearray(self.bin.read_bytes())
        data[0x200] ^= 1
        self.bin.write_bytes(data)
        r = self.precheck()
        self.assertEqual(r.returncode, 1)
        self.assertIn("is not", r.stdout)

    def test_a_truncated_bin_is_refused(self):
        self.bin.write_bytes(self.bin.read_bytes()[:-4])
        r = self.precheck()
        self.assertEqual(r.returncode, 1)
        self.assertIn("is not", r.stdout)


class FakeJob:
    """Records the commands; `on_wait` runs where the real one waits for DFU."""

    def __init__(self, bench, fail_download=False):
        self.bench, self.fail_download = bench, fail_download
        self.cmds, self.flashed, self.cancellable = [], [], True

    def say(self, line):
        pass

    def run(self, *cmd, cwd=None, env=None):
        self.cmds.append([str(c) for c in cmd])
        if cmd[0] == self.bench.DFU and cmd[1] == "-D":
            self.flashed.append(sha256(cmd[2]))
            if self.fail_download:
                raise RuntimeError("download failed")


class Bench(unittest.TestCase):
    def setUp(self):
        self.bench = load("bench")
        b = self.bench
        self.dir = Path(tempfile.mkdtemp())
        b.BENCH = self.dir
        b.FROZEN_ELF, b.FROZEN_BIN = self.dir / "akko-rmk.elf", self.dir / "akko-rmk.bin"
        b.STAMP, b.FLASHED = self.dir / "akko-rmk.json", self.dir / "flashed.json"
        b.FLASH_INCOMPLETE = self.dir / "flash-incomplete"
        b.FROZEN_ELF.write_bytes(b"elf")
        b.FROZEN_BIN.write_bytes(b"tested image")
        b.STAMP.write_text(b.json.dumps({"sha256": sha256(b.FROZEN_BIN), "git": "x"}))
        b.keyboard_state = lambda: {"mode": "dfu"}
        b.time.sleep = lambda s: None

    def tearDown(self):
        shutil.rmtree(self.dir)

    def test_a_frozen_image_swapped_during_the_wait_isnt_flashed(self):
        b = self.bench
        tested = sha256(b.FROZEN_BIN)

        def swap(job):
            b.FROZEN_BIN.write_bytes(b"something else")

        b.wait_for_dfu = swap
        job = FakeJob(b)
        b.do_flash_rmk(job)
        self.assertEqual(job.flashed, [tested])
        precheck = [c for c in job.cmds if c[-3:-2] == [str(TOOLS.relative_to(ROOT) / "precheck.py")]]
        self.assertTrue(precheck and not precheck[0][-1].startswith(str(b.FROZEN_BIN)))

    def test_after_a_failed_flash_leave_dfu_is_refused_until_one_succeeds(self):
        b = self.bench
        b.wait_for_dfu = lambda job: None
        with self.assertRaises(RuntimeError):
            b.do_flash_rmk(FakeJob(b, fail_download=True))
        self.assertTrue(b.FLASH_INCOMPLETE.exists())
        job = FakeJob(b)
        with self.assertRaisesRegex(RuntimeError, "didn't finish"):
            b.do_reset(job)
        self.assertEqual(job.cmds, [])
        b.do_flash_rmk(job)
        self.assertFalse(b.FLASH_INCOMPLETE.exists())
        b.do_reset(job)
        self.assertEqual(job.cmds[-1], [b.DFU, "-R"])


if __name__ == "__main__":
    unittest.main()
