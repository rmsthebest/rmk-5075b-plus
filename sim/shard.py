#!/usr/bin/env python3
"""Splits Robot suites into one suite per test, so renode-test -j runs them
in parallel (it only parallelises across suite files). Renode resets the
emulation before every test, so the tests don't depend on each other.

    shard.py OUT_DIR [--match REGEX] [--durations FILE] SUITE...

Each shard is OUT_DIR/<suite>-NN.robot: the suite's other sections as they
are (${CURDIR} pinned to the suite's own directory) plus one test case.
--match keeps only tests whose name matches REGEX (case-insensitive).
Prints the shard paths, longest-running tests first: by --durations
(test name -> seconds, from earlier runs: sim/durations.py), else by suite.
"""

import argparse
import json
import re
import sys
from pathlib import Path

SECTION = re.compile(r"^\*\*\*\s*([^*]+?)\s*\*\*\*", re.M)
# Run these suites' tests first: their tests are the longest, and starting
# them early keeps them from finishing last.
SLOW_FIRST = ("m4_rmk", "m3_usb")


def split_suite(path):
    """(text without test cases, [(name, test case text)])."""
    text = path.read_text()
    marks = list(SECTION.finditer(text))
    rest, tests = text[:marks[0].start()] if marks else text, []
    for i, m in enumerate(marks):
        end = marks[i + 1].start() if i + 1 < len(marks) else len(text)
        body = text[m.start():end]
        if m.group(1).lower().replace(" ", "") not in ("testcases", "testcase"):
            rest += body
            continue
        lines = body.split("\n")[1:]
        current = None
        for line in lines:
            if line and not line[0].isspace() and not line.startswith("#"):
                current = [line]
                tests.append((line.strip(), current))
            elif current is not None:
                current.append(line)
    return rest, [(name, "\n".join(lines).rstrip() + "\n") for name, lines in tests]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--match")
    ap.add_argument("--durations")
    ap.add_argument("suites", nargs="+")
    args = ap.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    match = re.compile(args.match, re.I) if args.match else None

    suites = sorted((Path(s).resolve() for s in args.suites),
                    key=lambda p: (p.stem not in SLOW_FIRST, SLOW_FIRST.index(p.stem) if p.stem in SLOW_FIRST else 0))
    shards = []
    for suite in suites:
        rest, tests = split_suite(suite)
        rest = rest.replace("${CURDIR}", str(suite.parent))
        for i, (name, body) in enumerate(tests, 1):
            if match and not match.search(name):
                continue
            shard = out / f"{suite.stem}-{i:02d}.robot"
            shard.write_text(f"{rest.rstrip()}\n\n*** Test Cases ***\n{body}")
            shards.append((name, shard))
    if not shards:
        sys.exit(f"no tests{' match ' + repr(args.match) if match else ''}")
    try:
        took = json.loads(Path(args.durations).read_text()) if args.durations else {}
    except (OSError, ValueError):
        took = {}
    if took:
        # A new test: assume it's slow, so it doesn't finish last.
        unknown = max(took.values())
        shards.sort(key=lambda s: -took.get(s[0], unknown))
    print("\n".join(str(shard) for _, shard in shards))


if __name__ == "__main__":
    main()
