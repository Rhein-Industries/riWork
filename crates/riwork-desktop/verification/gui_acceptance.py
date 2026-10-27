#!/usr/bin/env python3
"""Launch a disposable GUI fixture and retain evidence; never use live RiWork state.

Example (after building the branch, never an installed binary):
  python3 crates/riwork-desktop/verification/gui_acceptance.py \
    --binary target/debug/riwork --linger-seconds 90

The script prints a ready record containing the fixture PID and manifest path.
Inspect only that PID with RiWork's cua-driver MCP while both windows linger.
The fixture itself uses app.quit; no desktop automation provider is invoked here.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


PREFIX = "RIWORK_NSH_FIXTURE "


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def records_from(path):
    records = []
    for line in path.read_text(errors="replace").splitlines():
        if PREFIX in line:
            try:
                records.append(json.loads(line.split(PREFIX, 1)[1]))
            except json.JSONDecodeError:
                pass  # The host can still be writing the final log line.
    return records


def validate_records(records, project, state):
    failures = [r for r in records if r["kind"] == "failed"]
    if failures:
        raise RuntimeError(f"JS fixture failed: {failures}")
    params = [r["params"] for r in records if r["kind"] == "params"]
    if not any(r["kind"] == "checksPassed" for r in records) or len(params) < 2:
        return False
    assert len(params) == 2, f"unexpected bundle executions: {params}"
    assert {p["kind"] for p in params} == {"project", "global"}
    assert len({p["windowId"] for p in params}) == 2
    project_params = next(p for p in params if p["kind"] == "project")
    global_params = next(p for p in params if p["kind"] == "global")
    assert project_params["projectId"] == project["id"]
    assert project_params["path"] == project["root"]
    assert global_params["projectId"] is None
    assert global_params["path"] == str(state / "global-window")
    required = {"opened", "singleton", "listed", "placement", "routeReceived", "routed"}
    assert required <= {r["kind"] for r in records}
    return True


def registry_snapshot(runtime, pid, project, state):
    instances = []
    for path in (runtime / "instances").glob("*.json"):
        try:
            instance = json.loads(path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            continue
        instances.append(instance)
    matches = [i for i in instances if i["pid"] == pid]
    if len(matches) != 1 or len(matches[0]["windows"]) != 2:
        return None
    assert len(instances) == 1, "fixture registered more than one GUI process"
    instance = matches[0]
    assert instance["state_home"] == str(state)
    windows = instance["windows"]
    assert {w.get("kind", "project") for w in windows} == {"project", "global"}
    assert any(w["project_id"] == project["id"] and w["path"] == project["root"] for w in windows)
    assert any(w["project_id"] is None and w["path"] == str(state / "global-window") for w in windows)
    return instance


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--linger-seconds", type=int, default=90)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("AppKit acceptance must run on macOS")
    if not 5 <= args.linger_seconds <= 600:
        parser.error("--linger-seconds must be between 5 and 600")
    # Tool-launched processes can inherit blocked termination signals. Restore
    # the normal test-process mask before launching any isolated fixture child.
    signal.pthread_sigmask(signal.SIG_SETMASK, [])
    binary = args.binary.resolve(strict=True)
    # Never accept a home override: every run allocates a fresh short socket path.
    root = Path(tempfile.mkdtemp(prefix="rwnsh-", dir="/tmp")).resolve()
    os.chmod(root, 0o700)
    state, runtime, user_home, project_path = [root / name for name in ("state", "runtime", "user", "project")]
    for path in (state, user_home, project_path):
        path.mkdir(mode=0o700)
    environment = os.environ.copy()
    environment.update({"HOME": str(user_home), "RIWORK_HOME": str(state), "RIWORK_RUNTIME_DIR": str(runtime), "RIWORK_NSH_BACKGROUND": "1", "RUST_LOG": "warn,js=warn"})
    for key in ("RIWORK_RESTORE_TICKET", "RIWORK_CHAT_ID", "RIWORK_SHELL_ID", "RIWORK_NSH_BUNDLE"):
        environment.pop(key, None)
    manifest_path = root / "manifest.json"
    manifest = {"verification_environment": "isolated fixture only", "fixture_root": str(root), "binary": str(binary), "environment": {key: environment[key] for key in ("HOME", "RIWORK_HOME", "RIWORK_RUNTIME_DIR", "RIWORK_NSH_BACKGROUND")}, "commands": [], "status": "preparing"}
    write_json(manifest_path, manifest)

    def cli(arguments, name):
        result = subprocess.run([str(binary), *arguments], cwd=project_path, env=environment, input=b"", capture_output=True, timeout=30)
        (root / f"{name}.stdout").write_bytes(result.stdout)
        (root / f"{name}.stderr").write_bytes(result.stderr)
        manifest["commands"].append({"argv": [str(binary), *arguments], "exit_code": result.returncode, "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(), "stderr_sha256": hashlib.sha256(result.stderr).hexdigest()})
        assert result.returncode == 0, f"{arguments}: {result.stderr.decode(errors='replace')}"
        return result.stdout

    child = None
    try:
        project = json.loads(cli(["project", "add", str(project_path), "--name", "NSH isolated fixture", "--json"], "project-add"))
        cli(["help"], "help")
        cli(["project", "list"], "project-list")
        cli(["project", "list", "--json"], "project-list-json")
        assert not runtime.exists(), "CLI touched GUI runtime state"
        assert not (user_home / "Documents/riwork").exists(), "CLI created GUI project defaults"
        fixture = {"stateHome": str(state), "projectId": project["id"], "projectPath": project["root"], "lingerSeconds": args.linger_seconds}
        source = Path(__file__).with_name("gui_fixture.js").read_text()
        bundle = root / "bundle.js"
        bundle.write_text(source.replace("__FIXTURE_JSON__", json.dumps(fixture)))
        environment["RIWORK_NSH_BUNDLE"] = str(bundle)
        manifest["environment"]["RIWORK_NSH_BUNDLE"] = str(bundle)
        manifest["project"] = project
        manifest["gui_argv"] = [str(binary), str(project_path)]
        stderr_path = root / "gui.stderr"
        with (root / "gui.stdout").open("wb") as stdout, stderr_path.open("wb") as stderr:
            child = subprocess.Popen(manifest["gui_argv"], cwd=project_path, env=environment, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            manifest.update({"pid": child.pid, "status": "starting"})
            write_json(manifest_path, manifest)
            print(json.dumps({"status": "starting", "pid": child.pid, "manifest": str(manifest_path)}), flush=True)
            deadline = time.monotonic() + 30
            snapshot = None
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError(f"host exited before acceptance: {child.returncode}; see {stderr_path}")
                records = records_from(stderr_path)
                if validate_records(records, project, state):
                    snapshot = registry_snapshot(runtime, child.pid, project, state)
                    if snapshot:
                        break
                time.sleep(0.1)
            assert snapshot is not None, f"host did not pass acceptance in 30 seconds; see {stderr_path}"
            write_json(root / "registry.json", snapshot)
            write_json(root / "bridge-records.json", records)
            manifest.update({"status": "ready", "assertions": "distinct project/global params; window.open; singleton reuse; window.list; placement; route event; two native runtime windows"})
            write_json(manifest_path, manifest)
            print(json.dumps({"status": "ready", "pid": child.pid, "manifest": str(manifest_path), "window_ids": [r["params"]["windowId"] for r in records if r["kind"] == "params"], "linger_seconds": args.linger_seconds}), flush=True)
            child.wait(timeout=args.linger_seconds + 15)
            assert child.returncode == 0, f"host exited with {child.returncode}"
        records = records_from(stderr_path)
        assert any(r["kind"] == "quitting" for r in records), "bundle did not initiate app.quit"
        assert not list((runtime / "instances").glob("*.json")), "host failed to unregister on quit"
        assert not (state / "run/desktop.sock").exists(), "host left desktop socket behind"
        write_json(root / "bridge-records.json", records)
        manifest.update({"status": "passed", "gui_exit_code": child.returncode, "unregistered": True})
        write_json(manifest_path, manifest)
        print(json.dumps({"status": "passed", "manifest": str(manifest_path), "verification_environment": manifest["verification_environment"]}), flush=True)
    except Exception as error:
        manifest.update({"status": "failed", "error": str(error)})
        write_json(manifest_path, manifest)
        print(json.dumps({"status": "failed", "error": str(error), "manifest": str(manifest_path)}), file=sys.stderr, flush=True)
        raise
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    main()
