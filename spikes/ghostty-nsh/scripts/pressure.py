#!/usr/bin/env python3
"""Continuous real pane output, nominal 4 MiB/s, including while detached."""
import json
import os
import pathlib
import signal
import sys
import time

home = pathlib.Path(sys.argv[1]).resolve()
assert (home / 'GHOSTTY_NSH_SPIKE_FIXTURE').is_file()
running = True


def stop(*_args):
    global running
    running = False


for sig in (signal.SIGTERM, signal.SIGHUP):
    signal.signal(sig, stop)
payload = (b'Ghostty teardown pressure: 0123456789 abcdefghijklmnopqrstuvwxyz\r\n' * 1024)
sequence = total = 0
started = time.monotonic()
try:
    while running:
        remaining = memoryview(payload)
        while remaining and running:
            count = os.write(1, remaining)
            total += count
            remaining = remaining[count:]
        sequence += 1
        status = dict(pid=os.getpid(), sequence=sequence, bytes=total,
                      elapsed_s=time.monotonic()-started, monotonic_ms=time.monotonic()*1000,
                      nominal_bytes_per_s=4*1024*1024)
        temporary = home / 'pressure.tmp'
        temporary.write_text(json.dumps(status) + '\n')
        temporary.replace(home / 'pressure.json')
        time.sleep(len(payload)/(4*1024*1024))
finally:
    (home / 'pressure-stopped.json').write_text(json.dumps(status) + '\n')
