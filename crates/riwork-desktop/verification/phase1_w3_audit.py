#!/usr/bin/env python3
"""Structural W3 checks supplement the runtime and headless acceptance routes."""
import json
from pathlib import Path
import re
import subprocess

def git(*args):
    return subprocess.check_output(["git", *args], text=True)

ui = Path("apps/riwork")
native = Path("crates/riwork-nsh-native/src")
polling = []
for path in ui.rglob("*"):
    if path.suffix in (".tsx", ".ts") and ".nsh" not in path.parts and ".test." not in path.name:
        if re.search(r"setInterval\s*\(|requestAnimationFrame\s*\(|\.refetch\s*\(", path.read_text()):
            polling.append(str(path))
assert not polling, polling
storage = []
publishing = []
paths = [*native.glob("*.rs"), *ui.glob("*.tsx"), *ui.glob("*.ts"), *ui.joinpath("src").glob("*.rs")]
for path in paths:
    source = path.read_text()
    if re.search(r'(?:from\s+["\']nsh/storage|nsh_storage|nsh::storage)', source):
        storage.append(str(path))
    if re.search(r"appearance_file::(?:publish|write)|appearance_file::\{[^}]*publish", source):
        publishing.append(str(path))
assert not storage, storage
assert not publishing, publishing
diff = git("diff", "ed1b456", "--", "crates/riwork-core", "crates/riwork-engine")
format_changes = [line for line in diff.splitlines() if line[:1] in ("+", "-")
                  and not line.startswith(("+++", "---"))
                  and re.search(r"#\[serde|derive\([^)]*(?:Serialize|Deserialize)|SCHEMA_VERSION", line)]
assert not format_changes, format_changes
protected = git("ls-tree", "-r", "--name-only", "ea0f2f5", "--", "crates/riwork-desktop/src").splitlines()
assert not git("diff", "ea0f2f5", "--", *protected), "protected GPUI source changed"
print(json.dumps({"ui_polling": polling, "native_storage": storage,
    "appearance_publishers": publishing, "core_engine_serde_changes": format_changes,
    "protected_gpui_files_byte_identical": len(protected)}, indent=2))
