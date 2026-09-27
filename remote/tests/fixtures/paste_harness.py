"""Isolated raw PTY composer: brackets + Codex's 120ms paste/Enter suppression.

It never calls a model or creates a worker. Persist actual received submissions
so tests can assert complete 3500-character turns, exactly one Enter and no
cross-device interleaving. The old literal-text/immediate-Enter sequence is a
negative control: it inserts a newline rather than submitting during the burst.
"""
import json
import os
import select
import sys
import time
import tty

tty.setraw(0)
os.write(1, b"\x1b[?2004h")
state = {"turns": [], "enters": 0, "suppressed": 0, "bracketed": 0, "pid": os.getpid()}
pending = bytearray()
queue = bytearray()
last_text = 0.0
start, end = b"\x1b[200~", b"\x1b[201~"


def save():
    state["cells"] = list(os.get_terminal_size(0))
    path = sys.argv[1]
    tmp = path + ".tmp"
    with open(tmp, "w") as file:
        json.dump(state, file)
    os.replace(tmp, path)


save()
while True:
    if state["cells"] != list(os.get_terminal_size(0)):
        save()
    if not select.select([0], [], [], 0.050)[0]:
        continue
    queue.extend(os.read(0, 8192))
    while queue:
        if queue[0] == 27:
            if len(queue) < len(start):
                break
            if queue[:len(start)] == start:
                del queue[:len(start)]
                state["bracketed"] += 1
                continue
            if queue[:len(end)] == end:
                del queue[:len(end)]
                last_text = time.monotonic()
                continue
        char = queue.pop(0)
        if char == 13:
            state["enters"] += 1
            if time.monotonic() - last_text < 0.120:
                state["suppressed"] += 1
                pending.append(10)
            else:
                state["turns"].append(pending.decode("utf-8"))
                pending.clear()
            save()
        else:
            pending.append(char)
            last_text = time.monotonic()
