#!/usr/bin/env python3
"""Adds each test's run time in a Robot output.xml to a durations file
(test name -> seconds), for shard.py --durations.

    durations.py OUTPUT_XML DURATIONS_JSON
"""

import json
import sys
import xml.etree.ElementTree as ET
from datetime import datetime
from pathlib import Path


def seconds(status):
    if "elapsed" in status.attrib:
        return float(status.attrib["elapsed"])
    fmt = "%Y%m%d %H:%M:%S.%f"
    start, end = status.attrib.get("starttime"), status.attrib.get("endtime")
    if not start or not end or "N/A" in (start, end):
        return None
    return (datetime.strptime(end, fmt) - datetime.strptime(start, fmt)).total_seconds()


def main():
    out, path = sys.argv[1], Path(sys.argv[2])
    try:
        took = json.loads(path.read_text())
    except (OSError, ValueError):
        took = {}
    for test in ET.parse(out).getroot().iter("test"):
        status = test.find("status")
        t = seconds(status) if status is not None else None
        if t is not None:
            took[test.attrib["name"]] = round(t, 1)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(took, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
