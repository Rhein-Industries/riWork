#!/usr/bin/python3
"""A private stand-in for `riwork cua mcp` in the live Muse tests: it records
how Muse started it and what Muse asked, and serves one harmless tool. It never
touches the desktop. The record goes to $RIWORK_HOME/fake_cua_mcp.jsonl, the
RiWork home the driver names in the session's MCP configuration."""
import json
import os
import sys
import time

home = os.environ.get("RIWORK_HOME") or os.path.dirname(os.path.abspath(__file__))
record_path = os.path.join(home, "fake_cua_mcp.jsonl")


def record(entry):
    entry["pid"] = os.getpid()
    entry["t"] = time.time()
    with open(record_path, "a") as handle:
        handle.write(json.dumps(entry) + "\n")


record({"start": {"argv": sys.argv[1:], "cwd": os.getcwd(),
                  "env": {k: v for k, v in os.environ.items() if k.startswith("RIWORK")}}})
framing = None


def send(message):
    data = json.dumps(message).encode()
    if framing == "content-length":
        sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    else:
        sys.stdout.buffer.write(data + b"\n")
    sys.stdout.buffer.flush()


def read():
    global framing
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line.lower().startswith(b"content-length:"):
            framing = "content-length"
            length = int(line.split(b":", 1)[1])
            while sys.stdin.buffer.readline() not in (b"\r\n", b"\n", b""):
                pass
            return json.loads(sys.stdin.buffer.read(length))
        framing = framing or "lines"
        if line.strip():
            return json.loads(line)


while True:
    message = read()
    if message is None:
        record({"eof": True})
        break
    method = message.get("method")
    record({"recv": method})
    if "id" not in message:
        continue
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {
            "protocolVersion": message.get("params", {}).get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-cua-driver", "version": "0"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"tools": [{
            "name": "probe", "description": "Says ok (a test stand-in).",
            "inputSchema": {"type": "object", "properties": {}}}]}})
    elif method == "tools/call":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"content": [{"type": "text", "text": "ok"}]}})
    else:
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
