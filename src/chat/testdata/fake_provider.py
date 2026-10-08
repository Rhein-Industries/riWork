#!/usr/bin/env python3
"""A fake `codex app-server` / `claude` for the chat driver tests.

    fake_provider.py RECORD FIXTURE [FIXTURE ...] -- ARGS...

Each start of the fake takes the next FIXTURE (the last one again when there
are more starts than fixtures), so a test can script a restart. A fixture is
newline-delimited JSON, one step per line; blank lines and lines starting with
`#` are comments. The steps run in order:

  {"type":"emit","frame":{...}}        write the frame as one line to stdout
  {"type":"emit_raw","text":"..."}     write the text as one line (not JSON)
  {"type":"emit_oversized","bytes":N}  write one JSON line of N bytes
  {"type":"expect","frame":{...},      read one line from stdin and require that
   "reply":[{...},...],                it contains `frame` (a subset match, with
   "delay_ms":N}                       "<any>" matching any value), then, after
                                       the optional delay, emit the reply frames
  {"type":"expect_eof"}                wait for stdin to close
  {"type":"sleep","ms":N}
  {"type":"ignore_signals"}            keep running when SIGINT or SIGTERM arrives
  {"type":"hang"}                      do nothing until killed
  {"type":"attachment_backpressure"}   read a turn/start prefix, then stop draining
  {"type":"exit","code":N,             optionally write `stderr`, then exit
   "stderr":"..."}

In `emit` and `reply` frames the string "$id" stands for the `id` of the last
frame read, and "$request_id" for its `request_id`. After its last step the
fake stays alive until stdin closes (as the real programs do) and exits 0.

Everything is recorded, one JSON object per line, to RECORD: the start (argv,
working directory, some environment), every frame read as {"recv": frame},
every SIGINT or SIGTERM as {"signal": "INT", "ignored": false} (they end the
fake unless it was told to ignore them), and {"eof": true} when stdin closed.
A mismatch is recorded as {"mismatch": ...}
and exits with status 98; running out of time waiting for input exits 99.

The Codex driver asks `model/list` and `account/rateLimits/read` in the background. A step
that does not itself expect that request has it answered with an empty list (and
recorded like any frame read), so the fixtures that predate it need not mention it;
a fixture that wants real models expects it where it comes.
"""
import json
import os
import select
import signal
import sys

TIMEOUT = 20.0

record_path = sys.argv[1]
split = sys.argv.index("--")
fixtures = sys.argv[2:split]
args = sys.argv[split + 1:]


def record(entry):
    with open(record_path, "a") as handle:
        handle.write(json.dumps(entry) + "\n")


counter = record_path + ".starts"
try:
    with open(counter) as handle:
        start = int(handle.read() or 0) + 1
except FileNotFoundError:
    start = 1
with open(counter, "w") as handle:
    handle.write(str(start))

env = {}
for name, value in os.environ.items():
    if name.startswith(("CLAUDE_", "CODEX_", "ANTHROPIC_", "RIWORK_", "FAKE_")):
        secret = name.endswith("KEY") or "TOKEN" in name
        env[name] = "<set>" if secret else value
record({"start": {"n": start, "argv": args, "cwd": os.getcwd(), "env": env}})

fixture = fixtures[min(start, len(fixtures)) - 1]
steps = []
with open(fixture) as handle:
    for line in handle:
        line = line.strip()
        if line and not line.startswith("#"):
            steps.append(json.loads(line))

out = sys.stdout.buffer
last = {}
ignoring = False


def on_signal(number, _frame):
    name = signal.Signals(number).name[3:]
    record({"signal": name, "ignored": ignoring})
    if not ignoring:
        sys.exit(128 + number)


signal.signal(signal.SIGINT, on_signal)
signal.signal(signal.SIGTERM, on_signal)


def substitute(value):
    if value == "$id":
        return last.get("id")
    if value == "$request_id":
        return last.get("request_id")
    if isinstance(value, dict):
        return {key: substitute(item) for key, item in value.items()}
    if isinstance(value, list):
        return [substitute(item) for item in value]
    return value


def emit(frame):
    out.write(json.dumps(substitute(frame)).encode() + b"\n")
    out.flush()


def matches(expected, actual):
    if expected == "<any>":
        return True
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            key in actual and matches(item, actual[key]) for key, item in expected.items()
        )
    if isinstance(expected, list):
        return (
            isinstance(actual, list)
            and len(expected) == len(actual)
            and all(matches(a, b) for a, b in zip(expected, actual))
        )
    return expected == actual


buffer = b""


def read_line():
    """The next stdin line, or None at end of input."""
    global buffer
    while b"\n" not in buffer:
        ready, _, _ = select.select([sys.stdin.buffer], [], [], TIMEOUT)
        if not ready:
            record({"timeout": True})
            sys.exit(99)
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            return None
        buffer += chunk
    line, buffer = buffer.split(b"\n", 1)
    return line


for step in steps:
    kind = step["type"]
    if kind == "emit":
        emit(step["frame"])
    elif kind == "emit_raw":
        out.write(step["text"].encode() + b"\n")
        out.flush()
    elif kind == "emit_oversized":
        pad = max(step["bytes"] - len('{"pad":""}'), 0)
        out.write(b'{"pad":"' + b"x" * pad + b'"}\n')
        out.flush()
    elif kind == "expect":
        while True:
            line = read_line()
            if line is None:
                record({"mismatch": {"wanted": step["frame"], "got": "eof"}})
                sys.exit(98)
            frame = json.loads(line)
            record({"recv": frame})
            if frame.get("method") in ("model/list", "account/rateLimits/read") and step["frame"].get("method") != frame.get("method"):
                last = frame
                emit({"id": "$id", "result": {"data": [], "nextCursor": None} if frame.get("method") == "model/list" else {"rateLimits": None}})
                continue
            break
        if not matches(step["frame"], frame):
            record({"mismatch": {"wanted": step["frame"], "got": frame}})
            sys.exit(98)
        last = frame
        if step.get("delay_ms"):
            import time

            time.sleep(step["delay_ms"] / 1000.0)
        for reply in step.get("reply", []):
            emit(reply)
    elif kind == "expect_eof":
        while read_line() is not None:
            pass
        record({"eof": True})
    elif kind == "sleep":
        import time

        time.sleep(step["ms"] / 1000.0)
    elif kind == "ignore_signals":
        ignoring = True
    elif kind == "hang":
        while True:
            signal.pause()
    elif kind == "attachment_backpressure":
        # A deterministic proof of partial delivery: do not parse/read the giant
        # line, and never drain it after recording the prefix. Only fake runtimes.
        prefix, buffer = buffer, b""
        while b"turn/start" not in prefix:
            ready, _, _ = select.select([sys.stdin.buffer], [], [], TIMEOUT)
            if not ready:
                record({"timeout": True})
                sys.exit(99)
            chunk = os.read(sys.stdin.fileno(), 512)
            if not chunk or len(prefix) + len(chunk) > 65536:
                record({"mismatch": "attachment prefix missing"})
                sys.exit(98)
            prefix += chunk
        record({"backpressure": True, "prefix_bytes": len(prefix)})
        while True:
            signal.pause()
    elif kind == "exit":
        if step.get("stderr"):
            sys.stderr.write(step["stderr"] + "\n")
            sys.stderr.flush()
        sys.exit(step.get("code", 0))
    else:
        record({"mismatch": {"unknown step": step}})
        sys.exit(98)

# Stay alive like the real program until the driver closes our input.
while True:
    line = read_line()
    if line is None:
        record({"eof": True})
        break
    try:
        record({"recv": json.loads(line)})
    except ValueError:
        record({"recv_raw": line.decode(errors="replace")})
