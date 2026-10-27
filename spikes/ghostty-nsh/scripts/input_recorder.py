#!/usr/bin/env python3
"""A raw-mode application inside a real, isolated tmux pane. Throwaway only."""
import datetime
import json
import os
import pathlib
import re
import selectors
import signal
import sys
import termios
import time
import tty


home = pathlib.Path(sys.argv[1]).resolve()
if not (home / 'GHOSTTY_NSH_SPIKE_FIXTURE').is_file():
    raise SystemExit('Refusing to run outside the prepared fixture home')
log = (home / 'input-events.jsonl').open('a', buffering=1)
sequence = 0
resize_pending = True
stop_pending = False
mouse_buffer = b''


def record(kind, **fields):
    global sequence
    sequence += 1
    entry = {
        'seq': sequence,
        'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'monotonic_ns': time.monotonic_ns(),
        'kind': kind,
        'recorder_pid': os.getpid(),
        **fields,
    }
    log.write(json.dumps(entry, ensure_ascii=True) + '\n')
    log.flush()


def write(value):
    sys.stdout.buffer.write(value.encode() if isinstance(value, str) else value)
    sys.stdout.buffer.flush()


def on_resize(_signal, _frame):
    global resize_pending
    resize_pending = True


def on_stop(_signal, _frame):
    global stop_pending
    stop_pending = True


fd = sys.stdin.fileno()
original = termios.tcgetattr(fd)
signal.signal(signal.SIGWINCH, on_resize)
signal.signal(signal.SIGTERM, on_stop)
signal.signal(signal.SIGHUP, on_stop)
selector = selectors.DefaultSelector()
selector.register(fd, selectors.EVENT_READ)
record('start', tty=os.ttyname(fd), tmux=os.environ.get('TMUX'), pane=os.environ.get('TMUX_PANE'))
try:
    tty.setraw(fd)
    # Button/drag reporting, SGR coordinates, focus reporting, bracketed paste.
    write('\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[2J\x1b[H')
    write('Ghostty / NSH AppKit isolated tmux recorder\r\n')
    write('Type, paste, click/drag, resize; events go to input-events.jsonl.\r\n')
    write('The recorder PID and tmux pane must survive native-view detach/reattach.\r\n')
    write(f'Recorder PID: {os.getpid()} | pane: {os.environ.get("TMUX_PANE")}\r\n\r\n')
    while not stop_pending:
        if resize_pending:
            resize_pending = False
            size = os.get_terminal_size(fd)
            record('resize', columns=size.columns, rows=size.lines)
            write(f'RESIZE {size.columns}x{size.lines}\r\n')
        for _key, _mask in selector.select(0.2):
            payload = os.read(fd, 65536)
            if not payload:
                stop_pending = True
                break
            display = payload.decode('utf-8', errors='replace')
            record('input', hex=payload.hex(), utf8=display, byte_count=len(payload))
            write('INPUT ' + json.dumps(display, ensure_ascii=False)[:240] + '\r\n')
            mouse_buffer = (mouse_buffer + payload)[-4096:]
            consumed = 0
            for match in re.finditer(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])', mouse_buffer):
                record('mouse', button=int(match[1]), column=int(match[2]), row=int(match[3]), state='press-or-drag' if match[4] == b'M' else 'release')
                consumed = match.end()
            # Preserve only a possibly incomplete final SGR mouse sequence.
            mouse_buffer = mouse_buffer[consumed:]
            last_escape = mouse_buffer.rfind(b'\x1b[<')
            mouse_buffer = mouse_buffer[last_escape:] if last_escape >= 0 else b''
finally:
    record('stop')
    write('\x1b[?1002l\x1b[?1006l\x1b[?1004l\x1b[?2004l')
    termios.tcsetattr(fd, termios.TCSANOW, original)
    selector.close()
    log.close()
