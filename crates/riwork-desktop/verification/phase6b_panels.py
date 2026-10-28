#!/usr/bin/env python3
"""Windowless Phase 6b and existing screen suites with disposable state only.

No desktop application is launched. The generated NSH runner must enable the
closed test-support backends; no remote CLI, live host or scheduler is started.
Run under the assignment's serialized run.py to share its offline Cargo cache.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True, type=Path, help="new directory under /tmp")
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--nsh", required=True, type=Path)
    parser.add_argument("--tsc", required=True, type=Path)
    parser.add_argument("--command-log", type=Path)
    parser.add_argument("--filter")
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    if root.exists() or not root.is_relative_to(Path("/tmp").resolve()):
        parser.error("--root must be a new directory below /tmp")
    root.mkdir(mode=0o700, parents=True)
    repo = Path(__file__).resolve().parents[3]
    cargo = shutil.which("cargo")
    if not cargo:
        parser.error("cargo is required")
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(("RIWORK_", "CODEX_", "ANTHROPIC_", "OPENAI_")) or key in ("CARGO", "TMUX", "TMUX_TMPDIR"):
            env.pop(key, None)
    for directory in ("user", "runtime", "tmp", "tmux", "config", "data", "cache"):
        (root / directory).mkdir(mode=0o700)
    # Preserve only toolchain/cache locations supplied by the caller. Application
    # state, provider configuration and user files resolve to this private HOME.
    env.update(HOME=str(root / "user"), RIWORK_HOME=str(root / "store"),
               RIWORK_RUNTIME_DIR=str(root / "runtime"), TMPDIR=str(root / "tmp"),
               TMUX_TMPDIR=str(root / "tmux"), XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
               CARGO_TARGET_DIR=str(args.target_dir.resolve()), CARGO_BUILD_JOBS="2",
               CARGO_NET_OFFLINE="true", NSH_TSC=str(args.tsc.resolve()),
               RIWORK_NSH_FILES_FIXTURE="1", RIWORK_NSH_CHAT_FIXTURE="1")
    env.setdefault("NSH_CACHE", str(root / "nsh-cache"))
    command_log = args.command_log or root / "commands.jsonl"
    profile = '(version 1)(allow default)(deny network-inbound (local ip "*:*"))(deny network-outbound (remote ip "*:*"))'

    def run(argv, name):
        argv = [str(value) for value in argv]
        log_path = root / f"{name}.log"
        start = time.monotonic()
        with log_path.open("w") as log:
            result = subprocess.run(["/usr/bin/sandbox-exec", "-p", profile, *argv],
                                    cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                    stdout=log, stderr=subprocess.STDOUT)
        record = dict(label=name, argv=argv, exit_code=result.returncode,
                      elapsed_seconds=round(time.monotonic() - start, 2), log=str(log_path),
                      verification_environment="private HOME/RIWORK_HOME; windowless fixtures; IP network denied",
                      cargo_jobs=2)
        with command_log.open("a") as out:
            out.write(json.dumps(record) + "\n")
        print(json.dumps(record), flush=True)
        if result.returncode:
            print(log_path.read_text()[-10000:], flush=True)
            raise SystemExit(result.returncode)

    run([cargo, "run", "--locked", "--offline", "--jobs", "2", "-p", "riwork-nsh-native",
         "--features", "test-support", "--example", "p1_workspace_fixture_home", "--",
         "--scenario", "p1-layouts", "--out", root / "store"], "p6b-seed")
    fixture = json.loads((root / "store/fake-sessions.json").read_text())
    for row in fixture["sessions"]:
        row.update(userOpened=row.get("user_opened", False), projectId=row.get("project_id"),
                   worktreeId=row.get("worktree_id"), parentId=row.get("parent_id"),
                   unrestricted=False, orchestratorSkillLoaded=False, createdAtUnix=1)
        for key in ("command", "editorPath", "harness", "codexAccountId", "codexAccountLabel",
                    "codexAccountEmail", "codexHome", "orchestratorSkillVersion",
                    "orchestratorDelegationVersion", "orchestratorProjectRoot"):
            row[key] = None
        for key in ("user_opened", "project_id", "worktree_id", "parent_id"):
            row.pop(key, None)
    fixture["create_failures"] = fixture.get("create_errors", {})
    native = root / "store/native-fake-sessions.json"
    native.write_text(json.dumps(fixture, indent=2) + "\n")
    env["RIWORK_NSH_FAKE_SESSIONS"] = str(native)
    wrapper = root / "cargo-fixture"
    wrapper.write_text("#!/bin/sh\ncase \" $* \" in\n"
                       "*' -p nsh-app-riwork-nsh-native-test '*) exec " + shlex.quote(cargo) +
                       " \"$@\" --features app_riwork_nsh_native/test-support ;;\n"
                       "*) exec " + shlex.quote(cargo) + " \"$@\" ;;\nesac\n")
    wrapper.chmod(0o700)
    env["CARGO"] = str(wrapper)
    run([args.nsh, "check", "apps/riwork", "--tsc", args.tsc], "p6b-nsh-check")
    if not args.check_only:
        argv = [args.nsh, "test", "apps/riwork", "--junit", root / "nsh.xml"]
        if args.filter:
            argv += ["--filter", args.filter]
        run(argv, "p6b-nsh-test")
    files = [str(p) for p in (root / "tmux").rglob("*") if not p.is_dir() or p.is_symlink()]
    if files:
        raise RuntimeError(f"fixture created unexpected tmux files/sockets: {files}")
    print("No tmux files or sockets; no desktop windows or live targets.", flush=True)


if __name__ == "__main__":
    signal.pthread_sigmask(signal.SIG_SETMASK, [])
    main()
