"""Exercise the actual bundled Mermaid/WebKit worker, never a live RiWork window.

Usage: python3 macos/diagram/test_helper.py /path/to/riwork-diagram
No third-party Python packages or network rendering service are used.
"""
import base64
import http.server
import json
from pathlib import Path
import struct
import subprocess
import sys
import threading
import tempfile

ROOT = Path(__file__).resolve().parent
HELPER = sys.argv[1]
BUNDLE = (ROOT / "mermaid-11.17.2.min.js").read_text()
RENDERER = (ROOT / "render.js").read_text()

def render(source, dark=False):
    with tempfile.TemporaryDirectory(prefix="riwork-diagram-fixture-") as rules:
        request = {"source": source, "dark": dark, "bundle": BUNDLE, "renderer": RENDERER, "rules": rules}
        result = subprocess.run([HELPER], input=json.dumps(request).encode(), capture_output=True, timeout=18, check=True)
    return json.loads(result.stdout)

def image(result):
    assert "error" not in result, result
    png = base64.b64decode(result["png"], validate=True)
    assert png.startswith(b"\x89PNG\r\n\x1a\n")
    width, height = struct.unpack(">II", png[16:24])
    assert 1 <= width <= 2048 and 1 <= height <= 2048
    assert len(png) > 500, "diagram must contain meaningful raster content"
    return png

examples = {
    "flowchart": "flowchart LR\n A[Start é] --> B{Ready?}\n B -->|yes| C[Done]",
    "sequence": "sequenceDiagram\n Alice->>Bob: Hello\n Bob-->>Alice: Hi!",
    "class": "classDiagram\n class Animal {+String name}\n Animal <|-- Duck",
    "state": "stateDiagram-v2\n [*] --> Still\n Still --> Moving\n Moving --> [*]",
    "gantt": "gantt\n dateFormat YYYY-MM-DD\n title Plan\n section Work\n Design :a, 2026-01-01, 2d\n Build :after a, 3d",
    "pie": 'pie title Results\n "Passed" : 8\n "Failed" : 2',
    "mindmap": "mindmap\n root((Plan))\n  Build\n  Test\n  Release",
    "er": "erDiagram\n CUSTOMER ||--o{ ORDER : places",
}
for name, source in examples.items():
    image(render(source))
    print(name, "ok", flush=True)
assert image(render(examples["flowchart"], True)) != image(render(examples["flowchart"], False))
assert "error" in render("not a valid Mermaid diagram")
assert "error" in render("x" * 32769)

requests = []
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append(self.path)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"blocked resource")
    def log_message(self, *_):
        pass

server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
url = f"http://127.0.0.1:{server.server_port}/must-not-load"
render(f'%%{{init: {{"securityLevel":"loose","themeCSS":"@import url({url});"}}}}%%\nflowchart LR\n A["<img src=\\"{url}\\">"] --> B\n click B "{url}"')
render(f'flowchart LR\n A-->B\n classDef remote fill:url({url});\n class A remote;')
server.shutdown()
assert requests == [], requests
print("dark theme, malformed/oversize source, and network isolation ok", flush=True)
