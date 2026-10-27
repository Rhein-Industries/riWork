#!/usr/bin/env python3
"""Reproduce the Phase 1 integration subset using disposable state only.

Run build, headless, parity and gui separately with the same --root to retain
every command/result. GUI mode launches the real bundle in two project windows,
prints the fixture PID for read-only cua-driver inspection, and sends SIGTERM
after --linger-seconds (or when <root>/gui-stop appears). It sends no GUI input.
The signal stop is fixture cleanup, not proof of graceful app.quit behavior.
The targeted mode reruns the three integration crates after a follow-up fix.
The nsh mode reruns just the isolated NSH check/test subset.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


REPO = Path(__file__).resolve().parents[3]
NSH = Path("/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh")
TSC = Path("/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc")
PACKAGES = ("riwork-core", "riwork-engine", "riwork-cli", "riwork-desktop",
            "riwork-nsh-native", "riwork-nsh-workspace")
ENV_KEYS = ("HOME", "PATH", "RIWORK_HOME", "RIWORK_RUNTIME_DIR", "TMPDIR",
            "TMUX_TMPDIR", "XDG_CONFIG_HOME", "CARGO_HOME", "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN", "CARGO_TARGET_DIR", "CARGO_BUILD_JOBS",
            "CARGO_NET_OFFLINE", "NSH_CACHE", "NSH_TSC", "RIWORK_NSH_BUNDLE",
            "RIWORK_NSH_BACKGROUND", "RIWORK_NSH_FAKE_SESSIONS")


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Verification:
    def __init__(self, args):
        self.args = args
        root = args.root or Path(tempfile.mkdtemp(prefix="rwnshi-", dir="/tmp"))
        root.mkdir(parents=True, exist_ok=True)
        self.root = root.resolve()
        if not self.root.is_relative_to(Path("/tmp").resolve()):
            raise ValueError("--root must resolve inside /tmp")
        self.target = (args.target_dir or self.root / "target").resolve()
        self.logdir = Path(tempfile.mkdtemp(prefix=f"{args.mode}-", dir=self.root))
        self.manifest_path = self.logdir / "manifest.json"
        # folder_trust tests deliberately distinguish HOME from temp_dir().
        (REPO / "target").mkdir(exist_ok=True)
        self.home = Path(tempfile.mkdtemp(prefix="nsh-integration-home-", dir=REPO / "target"))
        self.env = os.environ.copy()
        self.env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
        self.env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
        self.env.update(HOME=str(self.home), RIWORK_HOME=str(self.logdir / "state"),
                        RIWORK_RUNTIME_DIR=str(self.logdir / "runtime"), TMPDIR="/tmp",
                        TMUX_TMPDIR=str(self.logdir / "tmux"),
                        XDG_CONFIG_HOME=str(self.home / ".config"),
                        CARGO_TARGET_DIR=str(self.target), CARGO_BUILD_JOBS=str(args.jobs),
                        CARGO_NET_OFFLINE="true", NSH_CACHE=str(self.root / "nsh-cache"),
                        NSH_TSC=str(args.tsc.resolve()))
        for key in ("RIWORK_RESTORE_TICKET", "RIWORK_CHAT_ID", "RIWORK_SHELL_ID",
                    "RIWORK_NSH_BUNDLE", "RIWORK_NSH_SOURCE", "RIWORK_NSH_FAKE_SESSIONS"):
            self.env.pop(key, None)
        Path(self.env["TMUX_TMPDIR"]).mkdir()
        signal.pthread_sigmask(signal.SIG_SETMASK, [])
        self.manifest = {"verification_environment": "isolated fixture homes only",
                         "mode": args.mode, "repo": str(REPO), "root": str(self.root),
                         "target_dir": str(self.target), "commands": [], "status": "running"}
        self.save()
        print(json.dumps({"mode": args.mode, "root": str(self.root),
                          "manifest": str(self.manifest_path)}), flush=True)

    def save(self):
        write_json(self.manifest_path, self.manifest)

    def command(self, argv, name, *, env=None, cwd=REPO, check=True, timeout=None):
        argv = [str(arg) for arg in argv]
        env = env or self.env
        out, err = self.logdir / f"{name}.stdout", self.logdir / f"{name}.stderr"
        record = {"name": name, "argv": argv, "cwd": str(cwd),
                  "environment": {key: env[key] for key in ENV_KEYS if key in env},
                  "stdout": str(out), "stderr": str(err)}
        self.manifest["commands"].append(record)
        self.save()
        print(json.dumps({"running": name, "argv": argv}), flush=True)
        started = time.monotonic()
        with out.open("wb") as stdout, err.open("wb") as stderr:
            result = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                    stdout=stdout, stderr=stderr, timeout=timeout)
        record.update(exit_code=result.returncode, duration_seconds=round(time.monotonic() - started, 3),
                      stdout_sha256=digest(out), stderr_sha256=digest(err))
        self.save()
        print(json.dumps({"finished": name, "exit_code": result.returncode}), flush=True)
        if check and result.returncode:
            raise RuntimeError(f"{name} exited {result.returncode}; see {out} and {err}")
        return result.returncode, out.read_bytes(), err.read_bytes()

    def cargo(self, *args):
        return ["cargo", *args, "--locked", "--offline"]

    def toolchain(self):
        _, version, _ = self.command([self.args.nsh, "--version"], "nsh-version")
        if version.strip() != b"nsh 0.3.0 (76fa8a7)":
            raise RuntimeError("verification requires the pinned NSH 0.3.0 CLI")
        self.manifest["nsh_cli"] = {"path": str(self.args.nsh.resolve()),
                                    "sha256": digest(self.args.nsh)}
        self.command(["rustc", "--version"], "rustc-version")
        self.command(["cargo", "--version"], "cargo-version")

    def build(self):
        self.toolchain()
        self.command(self.cargo("build", "-p", "riwork-desktop", "-p", "riwork-cli"), "cargo-build")
        output = self.root / "bundle"
        self.command([self.args.nsh, "build", "apps/riwork", "--out", output], "nsh-build")
        proofs = []
        for name, flags in (("cargo-tree-binary", ["--target", "aarch64-apple-darwin", "--edges", "normal,build"]),
                            ("cargo-tree-full", [])):
            _, tree, _ = self.command(self.cargo("tree", "-p", "riwork-desktop", *flags), name)
            forbidden = [line for line in tree.decode().splitlines()
                         if "gpui" in line.lower() or "ghostty" in line.lower()]
            if forbidden:
                raise RuntimeError(f"forbidden dependencies in {name}: {forbidden}")
            proofs.append({"name": name, "gpui_or_ghostty_lines": forbidden})
        self.manifest["dependency_proof"] = proofs
        self.manifest["bundle_files"] = {str(p.relative_to(output)): digest(p)
                                           for p in output.rglob("*") if p.is_file()}

    def headless(self):
        self.toolchain()
        package_args = [arg for package in PACKAGES for arg in ("-p", package)]
        self.command(self.cargo("test", *package_args), "cargo-tests")
        self.nsh()

    def nsh(self):
        if self.args.mode == "nsh":
            self.toolchain()
        # Existing TSX fixtures use the stable fixture-project ID and this exact
        # production LayoutStore writer. Keep their mutable state separate.
        nsh_home = self.logdir / "nsh-home"
        nsh_tmux = self.logdir / "nsh-tmux"
        nsh_tmux.mkdir()
        self.command(self.cargo("run", "-p", "riwork-nsh-workspace", "--example", "seed_fixture")
                     + ["--", nsh_home], "seed-nsh-fixture")
        env = dict(self.env, RIWORK_HOME=str(nsh_home), TMUX_TMPDIR=str(nsh_tmux))
        self.command([self.args.nsh, "check", "apps/riwork", "--tsc", self.args.tsc],
                     "nsh-check", env=env)
        self.command([self.args.nsh, "test", "apps/riwork", "--junit", self.logdir / "nsh.xml"],
                     "nsh-test", env=env)
        self.assert_no_sessions(nsh_home, nsh_tmux)

    def targeted(self):
        self.command(self.cargo("test", "-p", "riwork-desktop", "-p", "riwork-nsh-native",
                                "-p", "riwork-nsh-workspace"), "cargo-integration-tests")

    def assert_no_sessions(self, state, tmux=None):
        tmux = tmux or Path(self.env["TMUX_TMPDIR"])
        entries = sorted(tmux.rglob("*"))
        self.manifest["tmux_inventory"] = {
            "root": str(tmux),
            "empty_directories": [str(path.relative_to(tmux)) for path in entries if path.is_dir()],
            "files_or_sockets": [str(path.relative_to(tmux)) for path in entries
                                 if not path.is_dir() or path.is_symlink()],
        }
        # A read-only tmux discovery probe can create its empty tmux-UID
        # directory. Preserve that fact, while refusing any socket/file.
        if self.manifest["tmux_inventory"]["files_or_sockets"]:
            raise RuntimeError(f"fixture unexpectedly created tmux files or sockets in {tmux}")
        forbidden = [name for name in ("appearance.json", "sessions.json", "chats")
                     if (state / name).exists()]
        if forbidden:
            raise RuntimeError(f"fixture wrote deferred state: {forbidden}")

    def parity(self):
        binary = (self.args.binary or self.target / "debug/riwork").resolve(strict=True)
        reference = (self.args.reference or self.target / "debug/riwork-cli").resolve(strict=True)
        # Both entry points use this unchanged CLI crate. Preserve the evidence
        # instead of rebuilding the old GPUI host just to invoke its dispatcher.
        _, diff, _ = self.command(["git", "diff", "ea0f2f5", "--", "crates/riwork-cli"], "cli-source-diff")
        if diff:
            raise RuntimeError("CLI sources differ from ea0f2f5; supply/review a baseline reference")
        self.manifest["cli_reference"] = {"binary": str(reference), "sha256": digest(reference),
                                          "source_baseline": "ea0f2f5", "source_diff_empty": True}
        root = self.logdir / "cli-fixture"
        root.mkdir()
        state, runtime, home = root / "state", root / "runtime", root / "user"
        project = root / "project"
        project.mkdir()
        home.mkdir()
        env = dict(self.env, HOME=str(home), RIWORK_HOME=str(state), RIWORK_RUNTIME_DIR=str(runtime),
                   RIWORK_NSH_BUNDLE=str(root / "must-not-load.nsh"))
        self.command([binary, "project", "add", project, "--name", "Alpha", "--json"],
                     "parity-seed", env=env)
        golden = root / "state-golden"
        shutil.copytree(state, golden)
        cases = [("project-list", ["project", "list", "--json"]),
                 ("settings-list", ["settings", "list", "--json"]),
                 ("instances", ["instances", "--json"]),
                 ("appearance", ["appearance", "--json"]),
                 ("unknown-command", ["nosuchcommand"])]
        results = []
        for name, arguments in cases:
            outputs = []
            for side, executable in (("reference", reference), ("new", binary)):
                shutil.rmtree(state)
                shutil.copytree(golden, state)
                if runtime.exists():
                    shutil.rmtree(runtime)
                outputs.append(self.command([executable, *arguments], f"parity-{name}-{side}",
                                            env=env, check=False, timeout=30))
            if outputs[0] != outputs[1]:
                raise RuntimeError(f"CLI byte parity failed for {arguments}")
            if (state / "nsh").exists() or (state / "run/desktop.lock").exists():
                raise RuntimeError("CLI command initialized GUI state")
            results.append({"argv": arguments, "exit_code": outputs[0][0],
                            "stdout_bytes": len(outputs[0][1]), "stderr_bytes": len(outputs[0][2]),
                            "byte_equal": True})
        self.manifest["cli_parity"] = results

    def gui(self):
        if sys.platform != "darwin":
            raise RuntimeError("the real fixture host requires macOS")
        binary = (self.args.binary or self.target / "debug/riwork").resolve(strict=True)
        bundle = (self.args.bundle or self.root / "bundle/bundle.js").resolve(strict=True)
        # Short sockets are essential on macOS; the shared evidence root can be
        # longer, so allocate this fixture independently and record its location.
        root = Path(tempfile.mkdtemp(prefix="rwnshi-gui-", dir="/tmp")).resolve()
        if self.args.workspace_fixture:
            theme_args = ["--theme", self.args.theme] if self.args.theme else []
            self.command(self.cargo("run", "-p", "riwork-nsh-native", "--features", "test-support", "--example", "p1_workspace_fixture_home")
                         + ["--", "--scenario", "p1-basic", "--out", root / "home", *theme_args], "seed-gui-fixture")
            source = json.loads((root / "home/fixtures.json").read_text())
            fixture = {"projects": [{"id": p["projectId"], "root": p["root"], "name": p["name"]}
                                    for p in source["fixtures"] if p["name"] in ("Alpha", "Beta")]}
        else:
            _, stdout, _ = self.command(self.cargo("run", "-p", "riwork-nsh-workspace", "--example", "integration_fixture")
                                       + ["--", root], "seed-gui-fixture")
            fixture = json.loads(stdout)
        projects = fixture["projects"]
        if len(projects) != 2:
            raise RuntimeError("integration_fixture must produce exactly two projects")
        state, runtime, home, tmux = root / "home", root / "run", root / "user", root / "tmux"
        for path in (home / ".config", tmux, root / "tmp", root / "evidence"):
            path.mkdir(parents=True, exist_ok=True)
        env = dict(PATH="/usr/bin:/bin:/usr/sbin:/sbin", HOME=str(home), RIWORK_HOME=str(state),
                   RIWORK_RUNTIME_DIR=str(runtime), TMPDIR=str(root / "tmp"), TMUX_TMPDIR=str(tmux),
                   XDG_CONFIG_HOME=str(home / ".config"), RIWORK_NSH_BACKGROUND="1", RIWORK_NSH_BUNDLE=str(bundle))
        if self.args.fake_sessions:
            fake = state / "fake-sessions.json"
            if not fake.exists():
                write_json(fake, {"sessions": []})
            env["RIWORK_NSH_FAKE_SESSIONS"] = str(fake)
        before = {str(p.relative_to(state)): digest(p) for p in state.rglob("*") if p.is_file()}
        self.manifest.update(fixture_root=str(root), fixture=fixture,
                             gui_environment={key: env[key] for key in ENV_KEYS if key in env},
                             bundle={"path": str(bundle), "sha256": digest(bundle)})
        stop = self.root / "gui-stop"
        if stop.exists():
            raise RuntimeError(f"remove the previous fixture stop marker: {stop}")
        log_path = root / "gui.log"
        child = None
        try:
            argv = [str(binary), projects[0]["root"]]
            with log_path.open("wb") as log:
                child = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.DEVNULL,
                                         stdout=log, stderr=subprocess.STDOUT)
                self.manifest.update(pid=child.pid, gui_argv=argv, gui_log=str(log_path))
                self.save()
                self.wait_registry(runtime, child, 1)
                self.command([binary, projects[1]["root"]], "forward-second-window", env=env, cwd=root, timeout=30)
                registered = self.wait_registry(runtime, child, 2, require_layouts=True)
                if {w["project_id"] for w in registered["windows"]} != {p["id"] for p in projects}:
                    raise RuntimeError("runtime window project identities do not match fixtures")
                write_json(root / "evidence/registry.json", registered)
                self.manifest.update(status="ready_for_read_only_cua", registry=registered)
                self.save()
                print(json.dumps({"ready": True, "pid": child.pid, "fixture_root": str(root),
                                  "manifest": str(self.manifest_path), "stop_marker": str(stop),
                                  "linger_seconds": self.args.linger_seconds,
                                  "cua_policy": "cua-driver MCP read-only; explicit fixture PID only"}), flush=True)
                deadline = time.monotonic() + self.args.linger_seconds
                while time.monotonic() < deadline and not stop.exists():
                    if child.poll() is not None:
                        raise RuntimeError(f"fixture exited unexpectedly: {child.returncode}")
                    time.sleep(0.2)
                child.terminate()
                try:
                    child.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
                    raise RuntimeError("fixture did not exit after SIGTERM")
                self.manifest.update(gui_exit_code=child.returncode,
                                     shutdown_verification="SIGTERM fixture cleanup; graceful lifecycle covered by Rust tests")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        after = {str(p.relative_to(state)): digest(p) for p in state.rglob("*") if p.is_file()}
        changed = sorted(name for name, hash_value in after.items() if before.get(name) != hash_value)
        allowed = {"layouts.json", "layouts.lock", "global-window.json", "settings.json", "state.json", "run/desktop.lock"}
        if self.args.fake_sessions:
            allowed.add("fake-sessions.json")
        forbidden = [name for name in changed if name not in allowed
                     and not name.startswith(("layouts.corrupt-", "project-tabs/", "nsh/"))]
        self.manifest.update(written_files=changed, disallowed_written_files=forbidden)
        self.assert_no_sessions(state, tmux)
        if forbidden:
            raise RuntimeError(f"GUI wrote outside Phase 1 allow-list: {forbidden}")

    @staticmethod
    def wait_registry(runtime, child, count, require_layouts=False):
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if child.poll() is not None:
                raise RuntimeError(f"fixture exited during startup: {child.returncode}")
            entries = []
            for path in (runtime / "instances").glob("*.json"):
                try:
                    value = json.loads(path.read_text())
                except (FileNotFoundError, json.JSONDecodeError):
                    continue
                if value.get("pid") == child.pid:
                    entries.append(value)
            if len(entries) == 1 and len(entries[0]["windows"]) == count:
                if not require_layouts or all(window.get("layout") is not None for window in entries[0]["windows"]):
                    return entries[0]
            time.sleep(0.1)
        raise RuntimeError(f"fixture did not publish {count} ready windows with layouts={require_layouts}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("build", "headless", "targeted", "nsh", "parity", "gui"))
    parser.add_argument("--root", type=Path, help="reusable evidence/build directory inside /tmp")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--nsh", type=Path, default=NSH)
    parser.add_argument("--tsc", type=Path, default=TSC)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--reference", type=Path, help="unchanged riwork-cli binary")
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--linger-seconds", type=int, default=120)
    parser.add_argument("--fake-sessions", action="store_true", help="GUI uses the debug-only isolated fake session adapter")
    parser.add_argument("--workspace-fixture", action="store_true", help="GUI uses generated p1-basic Alpha/Beta with fake sessions")
    parser.add_argument("--theme", choices=("ghostty", "native", "ri_work", "catppuccin", "tokyo_night", "gruvbox_light", "hermes"), help="workspace GUI fixture theme")
    args = parser.parse_args()
    if args.workspace_fixture:
        args.fake_sessions = True
    if not 5 <= args.linger_seconds <= 600:
        parser.error("--linger-seconds must be between 5 and 600")
    run = Verification(args)
    try:
        getattr(run, args.mode)()
        run.manifest["status"] = "passed"
    except Exception as error:
        run.manifest.update(status="failed", error=str(error))
        raise
    finally:
        run.save()
        print(json.dumps({"status": run.manifest["status"], "manifest": str(run.manifest_path)}), flush=True)


if __name__ == "__main__":
    main()
