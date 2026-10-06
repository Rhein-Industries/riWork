#!/usr/bin/python3
"""Prepare a disposable native Cua fixture; never starts an app or provider.
Usage: /usr/bin/python3 scripts/automations-ui-fixture.py /absolute/path/to/built/riwork
Launch the resulting unique bundle through RiWork's Cua.ai Driver MCP.
"""
import json
import os
import shlex
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import uuid
import time
from datetime import datetime, timezone, timedelta

binary = Path(sys.argv[1]).resolve(strict=True)
fixture = Path(tempfile.mkdtemp(prefix="rwa-ui-", dir="/tmp")).resolve()
home = fixture / "home"
root = fixture / "project"
root.mkdir()
home.mkdir(mode=0o700)
child_env = {**os.environ, "RIWORK_HOME": str(home), "RIWORK_RUNTIME_DIR": str(fixture / "runtime")}
project = json.loads(subprocess.check_output([str(binary), "project", "add", str(root), "--json"], env=child_env))
(home / "settings.json").write_text(json.dumps({"schema_version": 1, "theme": "native", "panel_tab_icons": False, "remember_window_size": True}))
layout = {"layout": {"pane": 1}, "panes": {"1": {"tabs": [{"kind": "panel", "panel": "projects"}], "active_tab_key": "panel:projects"}}, "active_pane": 1, "locked_panes": [], "panels_initialized": True, "sidebar_visible": False, "window_size": {"width": 1100, "height": 1000}}
(home / "layouts.json").write_text(json.dumps({"schema_version": 1, "projects": {project["id"]: layout}}))
bundle_id = "dev.riwork.automations.fixture." + uuid.uuid4().hex[:8]
bundle = Path.home() / "Applications" / ("RiWork Automations Fixture " + bundle_id.rsplit(".", 1)[1] + ".app")
macos = bundle / "Contents/MacOS"
macos.mkdir(parents=True)
shutil.copy2(binary, macos / "riwork-bin")
wrapper = macos / "riwork"
wrapper.write_text(f'''#!/bin/sh
export RIWORK_HOME={shlex.quote(str(home))}
export RIWORK_RUNTIME_DIR={shlex.quote(str(fixture / "runtime"))}
unset RIWORK_CHAT_ID RIWORK_SHELL_ID RIWORK_CODEX_SHELL_ID RIWORK_RESTORE_TICKET
exec {shlex.quote(str(macos / "riwork-bin"))} {shlex.quote(str(root))}
''')
wrapper.chmod(0o755)
with (bundle / "Contents/Info.plist").open("wb") as file:
    plistlib.dump({"CFBundleExecutable": "riwork", "CFBundleIdentifier": bundle_id, "CFBundleName": "Automations Fixture", "CFBundlePackageType": "APPL", "CFBundleVersion": "1", "NSHighResolutionCapable": True}, file)
subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
subprocess.run(["/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister", "-f", str(bundle)], check=True)
metadata = {"fixture": str(fixture), "home": str(home), "root": str(root), "project_id": project["id"], "bundle": str(bundle), "bundle_id": bundle_id}
if "--seed-result" in sys.argv[2:]:
    # Inert, paused result for checking Open chat without a model or Send.
    chat_id = str(uuid.uuid4())
    info = {"id": chat_id, "provider": "claude", "project_id": project["id"], "cwd": str(root), "title": "Inert automation result", "created_at_unix": int(time.time()), "state": {"state": "stopped"}, "approval_mode": "supervised", "fast": False}
    chat_dir = home / "chats" / chat_id
    chat_dir.mkdir(parents=True, mode=0o700)
    (chat_dir / "info.json").write_text(json.dumps(info))
    (chat_dir / "events.jsonl").write_text(json.dumps({"chat_id":chat_id,"seq":1,"event":{"event":"info","info":info}}) + "\n")
    at = (datetime.now(timezone.utc) + timedelta(days=1)).isoformat(timespec="seconds")
    schedule = json.loads(subprocess.check_output([str(binary), "automation", "create", "--new-chat", "--provider", "claude", "--scope", "project", "--project", project["id"], "--title", "Seeded result fixture", "--prompt", "Fixture text only; never dispatch.", "--at", at, "--json"], env=child_env))["schedule"]
    subprocess.check_output([str(binary), "schedule", "pause", schedule["id"], "--revision", "1", "--scope", "project", "--project", project["id"], "--shell", schedule["target"]["shell_id"], "--json"], env=child_env)
    ledger_path = home / "schedules.json"
    ledger = json.loads(ledger_path.read_text())
    ledger["schedules"][0]["last_run"] = {"due_at":int(time.time()),"observed_at":int(time.time()),"outcome":"submitted","message":"Seeded fixture outcome; no provider run.","created_chat_id":chat_id}
    ledger_path.write_text(json.dumps(ledger))
    metadata.update(seeded_chat_id=chat_id, seeded_schedule_id=schedule["id"])
(fixture / "fixture.json").write_text(json.dumps(metadata, indent=2))
print(json.dumps(metadata, indent=2))
