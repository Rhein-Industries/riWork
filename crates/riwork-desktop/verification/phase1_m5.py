#!/usr/bin/env python3
"""M5 evidence probes. No desktop input; Cua reads are a separate explicit-PID step.

The integration fixture is supplemental: it does not pretend to be the missing
p1-basic/p1-restore generator. Commands, environment, output and hashes are kept
in phase1_integration.Verification manifests. No installation or live state.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time

from phase1_integration import Verification, NSH, TSC, REPO, digest, write_json


def reference(v):
    if v.args.zig:
        v.env["ZIG"] = str(v.args.zig.resolve(strict=True))
        v.command([v.args.zig, "version"], "zig-version")
        v.manifest["reference_toolchain"] = {"ZIG": v.env["ZIG"], "sha256": digest(v.args.zig)}
    source = v.logdir / "reference"
    source.mkdir()
    archive = v.logdir / "ea0f2f5.tar"
    v.command(["git", "archive", "--format=tar", "--output", archive, "ea0f2f5"], "archive")
    with tarfile.open(archive) as tar:
        tar.extractall(source, filter="data")
    # The historical host is required by cli-diff, not a substituted dispatcher.
    v.command(["cargo", "build", "--manifest-path", source / "Cargo.toml",
               "--target-dir", v.root / "reference-target", "-p", "riwork-desktop",
               "--locked", "--offline"], "reference-build", check=False)
    env = dict(v.env, RIWORK_FRAME_TABS="24")
    v.manifest["performance_environment"] = {"RIWORK_FRAME_TABS": "24", "ZIG": env.get("ZIG", "zig from PATH")}
    v.command(["cargo", "test", "--manifest-path", source / "Cargo.toml",
               "--target-dir", v.root / "reference-target", "-p", "riwork-desktop",
               "--release", "--locked", "--offline", "measure_a_busy_workspace_frame",
               "--", "--ignored", "--nocapture"], "gpui-frame-report", env=env, check=False)


def ledger(v):
    _, names, _ = v.command(["git", "ls-tree", "-r", "--name-only", "ea0f2f5",
                            "--", "crates/riwork-desktop/src"], "legacy-files")
    paths = names.decode().splitlines()
    _, changes, _ = v.command(["git", "diff", "ea0f2f5", "--", *paths], "legacy-source-diff")
    v.manifest["deletion_embargo"] = {"protected_source_files": len(paths),
        "byte_identical": not changes, "ledger_complete": False,
        "note": "Keeping source satisfies deletion embargo, not successor-test completion."}
    v.command(["sw_vers"], "macos")
    v.command(["uname", "-m"], "architecture")
    v.command(["git", "rev-parse", "HEAD"], "source-commit")
    v.command(["rg", "-n", "setInterval\\(|requestAnimationFrame\\(|\\.refetch\\(",
               "apps/riwork", "-g", "!**/*.test.ts*"], "polling-review", check=False)
    v.command(["rg", "-n", "nsh/storage|appearance_file|sessions.json|chats/",
               "apps/riwork", "crates/riwork-nsh-native", "crates/riwork-desktop/src/nsh_host.rs"],
              "writes-review", check=False)


def probes(v):
    for scenario in ("p1-basic", "p1-layouts", "p1-restore", "p1-theme"):
        v.command(v.cargo("run", "-p", "riwork-nsh-native", "--features", "test-support",
                          "--example", "p1_fixture_home")
                  + ["--", "--scenario", scenario, "--out", v.logdir / scenario / "home"],
                  scenario, check=False)
    v.command(v.cargo("test", "-p", "riwork-nsh-native", "--features", "test-support", "--test", "hubs")
              + ["--", "two_engines_share_one_home_watcher_and_one_hub_per_topic"],
              "PROC-07-hubs", check=False)
    v.command(["rg", "-n", "perfSettled|surface.*monitor|performance_snapshot|performanceSnapshot",
               "crates/riwork-desktop/src/nsh_host.rs", "apps/riwork"], "PERF-01-monitor", check=False)


def fixture(v):
    binary = v.target / "debug/riwork"
    bundle = v.root / "bundle/bundle.js"
    for path in (binary, bundle):
        path.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="rwm5-", dir="/tmp")).resolve()
    _, stdout, _ = v.command(v.cargo("run", "-p", "riwork-nsh-workspace", "--example", "integration_fixture")
                             + ["--", root, *(["--appearance"] if v.args.existing_appearance else [])], "fixture-generate")
    data = json.loads(stdout)
    for path in (root / "user/.config", root / "run", root / "tmux", root / "tmp", root / "evidence"):
        path.mkdir(parents=True, exist_ok=True)
    env = dict(PATH="/usr/bin:/bin:/usr/sbin:/sbin", HOME=str(root / "user"),
               TMPDIR=str(root / "tmp"), TMUX_TMPDIR=str(root / "tmux"),
               XDG_CONFIG_HOME=str(root / "user/.config"), RIWORK_HOME=str(root / "home"),
               RIWORK_RUNTIME_DIR=str(root / "run"), RIWORK_NSH_BACKGROUND="1",
               RIWORK_NSH_BUNDLE=str(bundle))
    projects = data["projects"]
    notadir = root / "notadir"
    notadir.write_text("x")
    code, _, err = v.command([binary, projects[0]["root"]], "PROC-03-notadir",
                             env=dict(env, RIWORK_HOME=str(notadir)), cwd=root, check=False, timeout=30)
    v.manifest["PROC-03"] = {"pass": code == 2 and err.startswith(b"riwork: "), "exit_code": code}
    v.command([binary, root / "missing"], "PROC-04-missing", env=env, cwd=root, check=False, timeout=30)
    state = root / "home"
    # Detect touched-but-byte-identical files too; preserve pre-existing state.
    def inventory():
        return {str(p.relative_to(state)): {"sha256": digest(p), "mtime_ns": p.stat().st_mtime_ns}
                for p in state.rglob("*") if p.is_file()}
    before = inventory()
    v.manifest.update(fixture_root=str(root), fixture=data, gui_environment=env,
                      binary={"path": str(binary), "sha256": digest(binary)},
                      bundle={"path": str(bundle), "sha256": digest(bundle)})
    child = None
    try:
        with (root / "gui.log").open("wb") as log:
            argv = [str(binary), projects[0]["root"]]
            child = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.DEVNULL,
                                     stdout=log, stderr=subprocess.STDOUT)
            v.manifest.update(pid=child.pid, gui_argv=argv)
            v.save()
            v.wait_registry(root / "run", child, 1, require_layouts=True)
            v.command([binary, "open", projects[1]["root"], "--json"], "PROC-05-open", env=env, cwd=root, check=False, timeout=30)
            registry = v.wait_registry(root / "run", child, 2, require_layouts=True)
            v.command([binary, "instances", "--json"], "PROC-06-instances", env=env, cwd=root, timeout=30)
            v.command([binary, "restart", "--json"], "RLD-01-restart", env=env, cwd=root, check=False, timeout=50)
            v.command([binary, "instances", "--json"], "RLD-01-after-instances", env=env, cwd=root, timeout=30)
            write_json(root / "evidence/registry.json", registry)
            v.manifest.update(status="ready_for_read_only_cua", registry=registry)
            v.save()
            print(json.dumps({"ready": True, "pid": child.pid, "fixture_root": str(root),
                              "manifest": str(v.manifest_path), "stop_marker": str(root / "stop")}), flush=True)
            deadline = time.monotonic() + v.args.linger_seconds
            while time.monotonic() < deadline and not (root / "stop").exists():
                if child.poll() is not None:
                    raise RuntimeError("fixture exited before inspection")
                time.sleep(.2)
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        v.manifest["fixture_exit_code"] = child.returncode if child else None
        after = inventory()
        changed = sorted(p for p in after if before.get(p) != after[p])
        allowed = {"layouts.json", "layouts.lock", "global-window.json", "settings.json", "state.json", "run/desktop.lock"}
        # Phase 5 (GLB-05, accepted): a dashboard/global read takes the engine's schedule
        # ledger lock as `riwork dashboard` does; only an empty schedules.lock is allowed.
        empty_lock = lambda p: p == "schedules.lock" and after[p]["sha256"] == hashlib.sha256(b"").hexdigest()
        bad = [p for p in changed if p not in allowed and not empty_lock(p)
               and not p.startswith(("project-tabs/", "nsh/"))
               and not (p.startswith("layouts.corrupt-") and p.endswith(".json") and "/" not in p)]
        removed = sorted(set(before) - set(after))
        v.manifest.update(written_files=changed, removed_files=removed, disallowed_written_files=bad,
                          before=before, after=after)
        v.save()
        # An intentionally preseeded appearance file is permitted only unchanged.
        if v.args.existing_appearance:
            v.manifest["appearance_unchanged"] = "appearance.json" in before and before["appearance.json"] == after.get("appearance.json")
            forbidden = [p for p in (state / "sessions.json", state / "chats") if p.exists()]
            entries = list((root / "tmux").rglob("*"))
            v.manifest["tmux_inventory"] = [str(p.relative_to(root / "tmux")) for p in entries]
            if forbidden or entries or not v.manifest["appearance_unchanged"]:
                raise RuntimeError("deferred-state write audit failed")
        else:
            v.assert_no_sessions(state, root / "tmux")
        if bad or removed:
            raise RuntimeError(f"write audit failed: writes={bad}, removed={removed}")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("mode", choices=("reference", "fixture", "ledger", "probes"))
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--target-dir", type=Path, required=True)
    p.add_argument("--linger-seconds", type=int, default=180)
    p.add_argument("--existing-appearance", action="store_true")
    p.add_argument("--zig", type=Path, help="existing Zig executable; never installed by this runner")
    p.add_argument("--jobs", type=int, default=2)
    p.add_argument("--nsh", type=Path, default=NSH)
    p.add_argument("--tsc", type=Path, default=TSC)
    args = p.parse_args()
    v = Verification(args)
    try:
        globals()[args.mode](v)
        v.manifest["status"] = "complete"
    except Exception as error:
        v.manifest.update(status="failed", error=str(error))
        raise
    finally:
        v.save()


if __name__ == "__main__":
    main()
