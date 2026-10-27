#!/usr/bin/env python3
"""Run the unchanged headless suites and new host tests with disposable state."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[3]
root = Path(tempfile.mkdtemp(prefix="rwnsh-tests-", dir="/tmp")).resolve()
# The existing folder_trust safety test assumes HOME is outside temp_dir().
# Give it a private HOME under the ignored build tree, never the real HOME.
(repo / "target").mkdir(exist_ok=True)
home = Path(tempfile.mkdtemp(prefix="nsh-test-home-", dir=repo / "target"))
env = os.environ.copy()
env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
env.update(HOME=str(home), RIWORK_HOME=str(root / "state"),
           RIWORK_RUNTIME_DIR=str(root / "runtime"), TMPDIR="/tmp")
for key in ("RIWORK_RESTORE_TICKET", "RIWORK_CHAT_ID", "RIWORK_SHELL_ID", "RIWORK_NSH_BUNDLE"):
    env.pop(key, None)
# Some automation hosts block SIGTERM/SIGINT. Child shutdown tests need the
# ordinary shell signal mask; unblock before launching Cargo and its children.
signal.pthread_sigmask(signal.SIG_SETMASK, [])
command = ["cargo", "test", "-p", "riwork-core", "-p", "riwork-engine",
           "-p", "riwork-cli", "-p", "riwork-desktop", "--offline"]
manifest = {"verification_environment": "isolated fixtures only", "cwd": str(repo),
            "argv": command, "environment": {k: env[k] for k in
            ("HOME", "RIWORK_HOME", "RIWORK_RUNTIME_DIR", "TMPDIR", "CARGO_HOME", "RUSTUP_HOME")}}
(root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"fixture": str(root), "log": str(root / "tests.log")}), flush=True)
with (root / "tests.log").open("w") as log:
    result = subprocess.run(command, cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT)
manifest["exit_code"] = result.returncode
(root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
for line in (root / "tests.log").read_text().splitlines():
    if line.startswith("test result:") or "FAILED" in line or line.startswith("error:"):
        print(line)
raise SystemExit(result.returncode)
