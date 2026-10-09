#!/usr/bin/env python3
"""Summarize GPUI ZED_MEASUREMENTS=1 logs split by PHASE <name> markers."""
import argparse
import json
import math
import re
import statistics
from pathlib import Path


def summarize(text):
    phases = []
    name = "startup"
    values = []

    def finish():
        if not values:
            return
        ordered = sorted(values)
        phases.append({
            "phase": name,
            "frames": len(values),
            "median_ms": statistics.median(ordered),
            "p95_ms": ordered[math.ceil(len(ordered) * 0.95) - 1],
            "max_ms": ordered[-1],
            "over_16_67_ms": sum(value > 1000 / 60 for value in ordered),
        })

    for line in text.splitlines():
        if line.startswith("PHASE "):
            finish()
            name = line[6:].strip()
            values = []
        else:
            match = re.fullmatch(r"frame duration: ([\d.]+)(ns|µs|us|ms|s)", line)
            if match:
                number, unit = match.groups()
                values.append(float(number) * {
                    "ns": 0.000001, "µs": 0.001, "us": 0.001,
                    "ms": 1, "s": 1000,
                }[unit])
    finish()
    return phases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    phases = summarize(args.log.read_text())
    if args.json:
        print(json.dumps(phases, indent=2))
    else:
        print("phase\tframes\tmedian ms\tp95 ms\tmax ms\t>16.67 ms")
        for phase in phases:
            print("{phase}\t{frames}\t{median_ms:.3f}\t{p95_ms:.3f}\t"
                  "{max_ms:.3f}\t{over_16_67_ms}".format(**phase))


if __name__ == "__main__":
    main()
