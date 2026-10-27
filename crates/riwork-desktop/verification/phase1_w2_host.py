#!/usr/bin/env python3
"""W2 real-host verification; isolated homes, explicit fixture PIDs, no GUI input.

The GUI mode pauses at before/after reload checkpoints for read-only Cua.
Create <logdir>/continue-before and continue-after after recording those reads.
Only owned fixture processes are cleaned up. SIGTERM cleanup is not quit proof.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import tempfile
import time

from phase1_integration import NSH, TSC, REPO, Verification, digest


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["build", "tests", "restore", "basic", "beta", "missing", "fail-flush", "auto-close", "auto-quit"])
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--theme")
    args = parser.parse_args()
    args.nsh, args.tsc = NSH, TSC
    run = Verification(args)
    # Cargo integration tests can replace target/debug/riwork with a different
    # feature graph. Keep this fixture-enabled executable immutable per build.
    binary = run.root / "fixture-bin/riwork"
    if args.mode == "build":
        run.command(run.cargo("build", "-p", "riwork-desktop", "--features", "riwork-nsh-native/test-support"), "cargo-build")
        binary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(run.target / "debug/riwork", binary)
        run.manifest["fixture_executable"] = {"path": str(binary), "sha256": digest(binary)}
        run.command([NSH, "build", "apps/riwork", "--out", run.root / "bundle"], "nsh-build")
        for mode, entry in [("auto-close", "AutoClose"), ("auto-quit", "AutoQuit")]:
            run.command([NSH, "build", f"apps/riwork/{entry}.fixture.tsx", "--out", run.root / mode], mode + "-build")
    elif args.mode == "tests":
        run.command(run.cargo("test", "-p", "riwork-desktop", "-p", "riwork-nsh-native", "-p", "riwork-nsh-workspace", "--features", "riwork-nsh-native/test-support"), "cargo-tests")
        run.command(run.cargo("test", "-p", "riwork-engine", "--features", "test-support", "--lib", "runtime::tests::"), "runtime-tests")
    else:
        scenario = "p1-restore" if args.mode in ("restore", "auto-close", "auto-quit") else "p1-basic"
        # Short paths keep desktop.sock within Darwin's sockaddr_un limit.
        root = Path(tempfile.mkdtemp(prefix="rww2-", dir="/tmp")).resolve()
        _, out, _ = run.command(run.cargo("run", "-p", "riwork-nsh-native", "--features", "test-support", "--example", "p1_fixture_home") + ["--", "--scenario", scenario, "--out", root / "home", "--executable", binary], "fixture")
        fixture = json.loads(out)
        (root / "user").mkdir()
        (root / "tmux").mkdir()
        (root / "tmp").mkdir()
        env = {"PATH": run.env["PATH"], "HOME": str(root / "user"),
               "RIWORK_HOME": fixture["home"], "RIWORK_RUNTIME_DIR": fixture["runtime"],
               "TMUX_TMPDIR": str(root / "tmux"), "XDG_CONFIG_HOME": str(root / "user/.config"),
               "TMPDIR": str(root / "tmp"),
               "RIWORK_NSH_BUNDLE": str(run.root / "bundle/bundle.js"), "RIWORK_NSH_BACKGROUND": "1",
               "RIWORK_NSH_FAKE_SESSIONS": fixture["fake_sessions"]}
        startup_env = dict(env)
        if args.theme:
            run.command([binary, "settings", "set", "theme", args.theme, "--json"], "set-theme", env=env)
        if args.mode == "fail-flush":
            startup_env["RIWORK_NSH_TEST_FAIL_DRAFT_FLUSH"] = "1"
        if args.mode.startswith("auto-"):
            startup_env["RIWORK_NSH_BUNDLE"] = str(run.root / args.mode / "bundle.js")
            startup_env["RUST_LOG"] = "info"
        if fixture.get("restore_ticket"):
            startup_env["RIWORK_RESTORE_TICKET"] = fixture["restore_ticket"]
        argv = [str(binary)]
        if args.mode == "basic":
            argv.append(fixture["projects"]["Alpha"]["root"])
        elif args.mode == "beta":
            argv.append(fixture["projects"]["Beta"]["root"])
        elif args.mode == "missing":
            argv.append(str(root / "missing"))
        stdout, stderr = run.logdir / "gui.stdout", run.logdir / "gui.stderr"
        launched = time.monotonic()
        process = subprocess.Popen(argv, cwd=REPO, env=startup_env, stdin=subprocess.DEVNULL,
                                   stdout=stdout.open("wb"), stderr=stderr.open("wb"))
        owned = {process.pid}
        run.manifest["gui"] = {"argv": argv, "environment": startup_env, "pid": process.pid,
                               "stdout": str(stdout), "stderr": str(stderr), "fixture": fixture,
                               "binary_sha256": digest(binary),
                               "bundle_sha256": digest(Path(startup_env["RIWORK_NSH_BUNDLE"]))}
        run.save()

        def instances(name):
            _, output, _ = run.command([binary, "instances", "--json"], name, env=env)
            data = json.loads(output)
            return data.get("instances", data) if isinstance(data, dict) else data

        def checkpoint(name):
            path = run.logdir / ("continue-" + name)
            print(json.dumps({"checkpoint": name, "manifest": str(run.manifest_path), "continue": str(path), "owned_pids": sorted(owned)}), flush=True)
            deadline = time.monotonic() + 300
            while not path.exists():
                if time.monotonic() >= deadline:
                    raise RuntimeError("Cua checkpoint timed out")
                time.sleep(.2)

        try:
            if args.mode.startswith("auto-"):
                process.wait(timeout=25)
                assert process.returncode == 0, stderr.read_text()
                run.manifest["lifetime_seconds"] = time.monotonic() - launched
                assert "requested for" in stdout.read_text() + stderr.read_text(), "lifecycle fixture never ran"
                assert not instances("instances-after-graceful-exit")
                run.manifest["graceful_exit_code"] = process.returncode
                run.manifest["status"] = "passed"
                run.save()
                return
            deadline = time.monotonic() + 18
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"fixture GUI exited {process.returncode}; {stderr.read_text()}")
                registrations = list(Path(fixture["runtime"]).glob("instances/*.json"))
                if any(json.loads(p.read_text()).get("windows") for p in registrations):
                    break
                time.sleep(.2)
            time.sleep(1)
            if args.mode == "basic":
                run.command([binary, "open", fixture["projects"]["Beta"]["root"], "--json"], "forward-beta", env=env)
                time.sleep(2)
            before = instances("instances-before")
            assert len(before) == 1 and before[0]["pid"] == process.pid, before
            run.manifest["before"] = before
            run.save()
            checkpoint("before")
            if args.mode == "restore":
                before = instances("instances-handoff-before")
                run.manifest["handoff_before"] = before
                _, output, _ = run.command([binary, "restart", "--json"], "restart", env=env, timeout=60)
                result = json.loads(output)
                run.manifest["reload"] = result
                owned.update(row["new_pid"] for row in result["reload"]["instances"] if row.get("new_pid"))
                # Confirmation reaches the CLI before the old host's next
                # worker turn closes AppViews and releases desktop ownership.
                process.wait(timeout=10)
                assert process.returncode == 0, process.returncode
                after = instances("instances-after")
                owned.update(row["pid"] for row in after)
                run.manifest["after"] = after
                run.save()
                assert len(after) == 1 and after[0]["pid"] != process.pid, after
                by_path = lambda rows: {row["path"]: row for row in rows}
                assert by_path(before[0]["windows"]) == by_path(after[0]["windows"]), "runtime window payload changed across restart"
                run.manifest["runtime_windows_equal"] = True
                run.manifest["old_gui_exit_code"] = process.returncode
                run.save()
                checkpoint("after")
            elif args.mode == "fail-flush":
                code, output, errors = run.command([binary, "restart", "--json"], "restart-failed-flush", env=env, timeout=60, check=False)
                assert code == 2 and b"fixture draft flush failure" in errors, (code, output, errors)
                after = instances("instances-after-failed-flush")
                assert len(after) == 1 and after[0]["pid"] == process.pid, after
                assert process.poll() is None
                run.command([binary, "open", fixture["projects"]["Beta"]["root"], "--json"], "forward-after-failed-flush", env=env)
                run.manifest["failed_flush_preserves_old_gui"] = True
                run.save()
                checkpoint("after")
        except BaseException as error:
            run.manifest["status"] = "failed"
            run.manifest["error"] = str(error)
            run.save()
            raise
        finally:
            # Read this private registry for replacements even when a command
            # failed, never discover or signal another home/process.
            active = {process.pid} if process.poll() is None else set()
            try:
                for row in instances("instances-cleanup"):
                    if Path(row["state_home"]) == Path(fixture["home"]):
                        owned.add(row["pid"])
                        active.add(row["pid"])
            except Exception:
                pass
            for pid in active:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            if process.poll() is None:
                process.wait(timeout=10)
            run.manifest["cleanup"] = {"method": "SIGTERM fixture PIDs only", "pids": sorted(active), "owned_pids": sorted(owned)}
            for attempt in range(10):
                remaining = instances(f"instances-after-cleanup-{attempt}")
                if not remaining:
                    break
                time.sleep(1)
            run.manifest["cleanup"]["remaining_instances"] = remaining
            if remaining:
                run.manifest["status"] = "failed"
                run.manifest["error"] = f"fixture instances survived cleanup: {remaining}"
            run.save()
            assert not remaining, f"fixture instances survived cleanup: {remaining}"
    run.manifest["status"] = "passed"
    run.save()
    print(json.dumps({"status": "passed", "manifest": str(run.manifest_path)}), flush=True)


if __name__ == "__main__":
    main()
