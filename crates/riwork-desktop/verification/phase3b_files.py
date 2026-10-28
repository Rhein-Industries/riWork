#!/usr/bin/env python3
"""Run windowless Files acceptance with both legacy and native fixture readers."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True, help="new directory below /tmp")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--nsh", type=Path, required=True)
    parser.add_argument("--tsc", type=Path, required=True)
    parser.add_argument("--filter", help="optional NSH test name filter")
    args = parser.parse_args()
    root = args.root.resolve()
    if root.exists() or not root.is_relative_to(Path("/tmp").resolve()):
        parser.error("--root must be a new directory below /tmp")
    root.mkdir(parents=True)
    repo = Path(__file__).resolve().parents[3]
    cargo = shutil.which("cargo")
    if not cargo:
        parser.error("cargo is required")
    env = os.environ.copy()
    for key in ("RIWORK_RESTORE_TICKET", "RIWORK_CHAT_ID", "RIWORK_SHELL_ID", "CARGO"):
        env.pop(key, None)
    for directory in ("runtime", "tmp", "tmux"):
        (root / directory).mkdir()
    env.update(RIWORK_HOME=str(root / "home"), RIWORK_RUNTIME_DIR=str(root / "runtime"),
               TMPDIR=str(root / "tmp"), TMUX_TMPDIR=str(root / "tmux"),
               CARGO_TARGET_DIR=str((args.target_dir or root / "target").resolve()),
               NSH_CACHE=str(root / "nsh-cache"), CARGO_BUILD_JOBS="4", CARGO_NET_OFFLINE="true",
               RIWORK_NSH_FILES_FIXTURE="1", RIWORK_NSH_CHAT_FIXTURE="1")
    commands = []

    def run(argv, name):
        argv = [str(value) for value in argv]
        commands.append({"argv": argv, "log": name + ".log"})
        (root / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        with (root / (name + ".log")).open("w") as log:
            result = subprocess.run(argv, cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT)
        print(f"{name}: exit {result.returncode}", flush=True)
        result.check_returncode()

    run([cargo, "run", "--locked", "--offline", "-p", "riwork-nsh-native", "--features",
         "test-support", "--example", "p1_workspace_fixture_home", "--", "--scenario",
         "p1-layouts", "--out", root / "home"], "seed")
    # Workspace's fixture reader has a small snake-case shell projection. The
    # native fake backend requires the full camel-case ShellSessionDto. Keep
    # their files separate; both inventories refer only to these fake sessions.
    fixture = json.loads((root / "home/fake-sessions.json").read_text())
    for old in fixture["sessions"]:
        old.update(userOpened=old.get("user_opened", False), projectId=old.get("project_id"),
                   worktreeId=old.get("worktree_id"), parentId=old.get("parent_id"),
                   unrestricted=False, orchestratorSkillLoaded=False, createdAtUnix=1)
        for key in ("command", "editorPath", "harness", "codexAccountId", "codexAccountLabel",
                    "codexAccountEmail", "codexHome", "orchestratorSkillVersion",
                    "orchestratorDelegationVersion", "orchestratorProjectRoot"):
            old[key] = None
        for key in ("user_opened", "project_id", "worktree_id", "parent_id"):
            old.pop(key, None)
    fixture["create_failures"] = fixture.get("create_errors", {})
    native = root / "home/native-fake-sessions.json"
    native.write_text(json.dumps(fixture, indent=2) + "\n")
    env["RIWORK_NSH_FAKE_SESSIONS"] = str(native)
    # NSH 0.3 has no app dependency feature option. Its public CARGO override
    # enables the test-only fake backend only in the generated headless runner.
    wrapper = root / "cargo-fixture"
    wrapper.write_text("#!/bin/sh\ncase \" $* \" in\n"
                       "*' -p nsh-app-riwork-nsh-native-test '*) exec " + shlex.quote(cargo) +
                       " \"$@\" --features app_riwork_nsh_native/test-support ;;\n"
                       "*) exec " + shlex.quote(cargo) + " \"$@\" ;;\nesac\n")
    wrapper.chmod(0o700)
    env["CARGO"] = str(wrapper)
    run([args.nsh, "check", "apps/riwork", "--tsc", args.tsc], "nsh-check")
    command = [args.nsh, "test", "apps/riwork", "--junit", root / "nsh.xml"]
    if args.filter:
        command += ["--filter", args.filter]
    run(command, "nsh-test")
    if any((root / "tmux").iterdir()):
        raise RuntimeError("the fixture created unexpected tmux state")


if __name__ == "__main__":
    signal.pthread_sigmask(signal.SIG_SETMASK, [])
    main()
