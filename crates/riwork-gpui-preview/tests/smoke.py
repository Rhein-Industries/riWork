#!/usr/bin/env python3
"""Exercise a real windowless GPUI child over the documented preview protocol."""

import base64
import json
import os
from pathlib import Path
import secrets
import socket
import struct
import subprocess
import sys
import tempfile
import time


def main():
    executable = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/examples/counter").resolve()
    with tempfile.TemporaryDirectory(prefix="rw-preview-") as directory:
        path = Path(directory) / "preview.sock"
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(path))
        listener.listen(1)
        listener.settimeout(15)
        token = secrets.token_hex(32)
        environment = dict(os.environ, RIWORK_PREVIEW_SOCKET=str(path), RIWORK_PREVIEW_TOKEN=token)
        child = subprocess.Popen([str(executable)], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        connection = None
        try:
            connection, _ = listener.accept()
            connection.settimeout(10)
            pending = bytearray()

            def read():
                while b"\n" not in pending:
                    part = connection.recv(65536)
                    if not part:
                        raise AssertionError("Child disconnected unexpectedly")
                    pending.extend(part)
                    if len(pending) > 16 * 1024 * 1024:
                        raise AssertionError("Unbounded message")
                end = pending.index(b"\n") + 1
                line = bytes(pending[:end])
                del pending[:end]
                return json.loads(line)

            def send(value):
                connection.sendall(json.dumps(value).encode() + b"\n")

            hello = read()
            assert hello == {"type": "hello", "token": token, "pid": child.pid, "protocol": 1}
            send({"type": "ready", "protocol": 1})

            def frame(width=None, height=None):
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    message = read()
                    assert message["type"] == "frame", message
                    png = base64.b64decode(message["png"], validate=True)
                    assert png[:8] == b"\x89PNG\r\n\x1a\n"
                    dimensions = struct.unpack(">II", png[16:24])
                    assert dimensions == (message["width"], message["height"])
                    if width is None or dimensions == (width, height):
                        return png
                raise AssertionError("Requested frame size never arrived")

            first = frame(960, 600)
            send({"type": "input", "input": {"kind": "click", "x": 80, "y": 132, "button": "left"}})
            assert frame() != first, "GPUI button click must change raster output"
            send({"type": "input", "input": {"kind": "text", "text": "Hello 👋 世界"}})
            send({"type": "input", "input": {"kind": "resize", "width": 800, "height": 450}})
            frame(800, 450)
            send({"type": "visibility", "visible": False})
            time.sleep(0.2)
            # Drain any frame already in flight before checking the hidden period.
            connection.settimeout(0.15)
            try:
                while True:
                    frame()
            except (TimeoutError, socket.timeout):
                pass
            connection.settimeout(0.3)
            try:
                data = connection.recv(1)
                raise AssertionError(f"Hidden preview unexpectedly sent data: {data!r}")
            except (TimeoutError, socket.timeout):
                pass
            connection.settimeout(10)
            send({"type": "visibility", "visible": True})
            frame(800, 450)
            send({"type": "shutdown"})
            assert child.wait(timeout=5) == 0
            print("PASS: authenticated socket; actual PNG frames; GPUI click; text; resize; hidden pause; shutdown")
        finally:
            if connection:
                connection.close()
            listener.close()
            if child.poll() is None:
                child.kill()
                child.wait()
            if child.returncode:
                print(child.stderr.read().decode(), file=sys.stderr)


if __name__ == "__main__":
    main()
