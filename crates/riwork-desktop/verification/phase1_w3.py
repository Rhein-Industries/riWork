#!/usr/bin/env python3
"""W3 reproducible checks. All mutable app state is isolated; no live UI input."""
import argparse
import hashlib
import json
import subprocess
import tempfile
import time
from pathlib import Path
from phase1_integration import Verification, NSH, TSC, digest


def fixture(v):
    v.command(v.cargo("build", "-p", "riwork-desktop"), "fixture-build")
    bundle = v.root / "bundle"
    v.command([v.args.nsh, "build", "apps/riwork", "--out", bundle], "fixture-bundle")
    root = Path(tempfile.mkdtemp(prefix="w3-theme-", dir="/tmp")).resolve()
    _, output, _ = v.command(v.cargo("run", "-p", "riwork-nsh-workspace", "--example", "integration_fixture") + ["--", root, "--appearance"], "fixture-seed")
    data = json.loads(output)
    home = root / "home"
    # An isolated fixture selects Native through the real CLI Settings owner.
    env = dict(PATH="/usr/bin:/bin:/usr/sbin:/sbin", HOME=str(root / "user"), RIWORK_HOME=str(home),
               RIWORK_RUNTIME_DIR=str(root / "run"), XDG_CONFIG_HOME=str(root / "config"),
               TMUX_TMPDIR=str(root / "tmux"), TMPDIR=str(root / "tmp"),
               RIWORK_NSH_BUNDLE=str(bundle / "bundle.js"), RIWORK_NSH_BACKGROUND="1",
               RIWORK_APPEARANCE="light")
    for key in ("HOME", "RIWORK_RUNTIME_DIR", "XDG_CONFIG_HOME", "TMUX_TMPDIR", "TMPDIR"):
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    binary = v.target / "debug/riwork"
    v.command([binary, "settings", "set", "theme", "native"], "fixture-native-theme", env=env)
    def inventory():
        return {str(path.relative_to(home)): {"sha256": digest(path), "mtime_ns": path.stat().st_mtime_ns}
                for path in home.rglob("*") if path.is_file()}
    before = inventory()
    v.manifest.update(fixture_root=str(root), fixture=data, gui_environment=env,
                      before=before, forced_appearance="light")
    child = None
    try:
        with (root / "gui.log").open("wb") as log:
            child = subprocess.Popen([str(binary), data["projects"][0]["root"]], env=env,
                                     cwd=root, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            v.manifest["pid"] = child.pid
            v.save()
            v.wait_registry(root / "run", child, 1, require_layouts=True)
            # Direct second-launch forwarding stays in this exact fixture binary.
            v.command([binary, data["projects"][1]["root"]], "fixture-second-window", env=env, cwd=root, timeout=30)
            v.manifest["registry"] = v.wait_registry(root / "run", child, 2, require_layouts=True)
            v.manifest["status"] = "ready_for_cua"
            v.save()
            print(json.dumps({"ready": True, "pid": child.pid, "fixture_root": str(root), "manifest": str(v.manifest_path)}), flush=True)
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline and not (root / "stop").exists():
                if child.poll() is not None:
                    raise RuntimeError("fixture exited before capture")
                time.sleep(.2)
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            child.wait(timeout=15)
        after = inventory()
        changed = sorted(path for path in after if after[path] != before.get(path))
        removed = sorted(set(before) - set(after))
        allowed = {"layouts.json", "layouts.lock", "global-window.json", "settings.json", "state.json", "run/desktop.lock"}
        # Phase 5 (GLB-05, accepted): a dashboard/global read takes the engine's schedule
        # ledger lock as `riwork dashboard` does; only an empty schedules.lock is allowed.
        empty_lock = lambda path: path == "schedules.lock" and after[path]["sha256"] == hashlib.sha256(b"").hexdigest()
        bad = [path for path in changed if path not in allowed and not path.startswith(("nsh/", "project-tabs/"))
               and not empty_lock(path)]
        unchanged = before.get("appearance.json") == after.get("appearance.json") and "appearance.json" in before
        tmux_files = [str(path) for path in (root / "tmux").rglob("*") if not path.is_dir()]
        v.manifest.update(after=after, changed=changed, removed=removed, disallowed_writes=bad,
                          appearance_unchanged=unchanged, tmux_files=tmux_files,
                          fixture_exit=child.returncode if child else None)
        v.save()
        if bad or removed or tmux_files or not unchanged or (home / "sessions.json").exists() or (home / "chats").exists():
            raise RuntimeError("fixture write audit failed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("rust", "nsh", "probe", "fixture", "review"))
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--nsh", type=Path, default=NSH)
    parser.add_argument("--tsc", type=Path, default=TSC)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    v = Verification(args)
    try:
        v.toolchain()
        if args.mode == "rust":
            v.command(v.cargo("test", "-p", "riwork-core", "--lib"), "core-tests")
            v.command(v.cargo("test", "-p", "riwork-nsh-workspace"), "workspace-rust-tests")
            v.command(v.cargo("test", "-p", "riwork-nsh-workspace", "--test", "layout_backend") + ["--", "--nocapture"], "sync-call-timings")
            v.command(v.cargo("test", "-p", "riwork-nsh-native", "--features", "test-support"), "native-tests")
            v.command(v.cargo("test", "-p", "riwork-desktop", "--bin", "riwork"), "host-unit-tests")
            v.command(["cargo", "fmt", "-p", "riwork-core", "-p", "riwork-nsh-native", "-p", "riwork-nsh-workspace", "-p", "riwork-desktop", "--check"], "format")
            v.command(["git", "diff", "--check"], "whitespace")
        elif args.mode == "nsh":
            v.nsh()
        elif args.mode == "fixture":
            fixture(v)
        elif args.mode == "review":
            v.command(v.cargo("test", "-p", "riwork-desktop", "--bin", "riwork"), "host-final-tests")
            v.command(v.cargo("test", "-p", "riwork-desktop", "--test", "appkit_shell") + ["--", "view_menu_text_size_items"], "view-menu-text-size-items")
            v.command(["cargo", "fmt", "-p", "riwork-core", "-p", "riwork-nsh-native", "-p", "riwork-nsh-native-test-support", "-p", "riwork-nsh-workspace", "-p", "riwork-desktop", "--check"], "final-format")
            v.command(["git", "diff", "--check"], "final-whitespace")
            v.command(["python3", "crates/riwork-desktop/verification/phase1_w3_audit.py"], "source-audit")
        else:
            app = "crates/riwork-nsh-native/tests/app"
            v.command([args.nsh, "check", app, "--tsc", args.tsc], "native-probe-check")
            v.command([args.nsh, "test", app, "--junit", v.logdir / "native-probe.xml"], "native-probe-test")
            v.assert_no_sessions(Path(v.env["RIWORK_HOME"]))
        v.manifest["status"] = "complete"
    except Exception as error:
        v.manifest.update(status="failed", error=str(error))
        raise
    finally:
        v.save()


if __name__ == "__main__":
    main()
