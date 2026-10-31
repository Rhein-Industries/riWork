#!/usr/bin/env python3
"""Run an isolated RiWork desktop with automatic TSX view reloads."""
import argparse
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import time


def run(command, *, cwd, env):
    print("+ " + " ".join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True)


def stop(process):
    if process is None or process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=repo, help="Project opened in the development window")
    parser.add_argument("--home", type=Path, help="Separate RiWork data directory (default: target/dev-ui/home)")
    parser.add_argument("--nsh", default=os.environ.get("NSH_CLI") or shutil.which("nsh"), help="Pinned NSH 0.3.0 CLI, or set NSH_CLI")
    parser.add_argument("--port", type=int, help="Local dev-server port (default: choose a free port)")
    args = parser.parse_args()
    if not args.nsh:
        parser.error("NSH 0.3.0 is required; pass --nsh or set NSH_CLI")
    nsh = Path(args.nsh).expanduser().resolve()
    version = subprocess.check_output([str(nsh), "--version"], text=True).strip()
    if version.split()[:2] != ["nsh", "0.3.0"]:
        parser.error(f"Expected NSH 0.3.0, found {version}; pass the pinned CLI with --nsh or NSH_CLI")
    project = args.project.expanduser().resolve(strict=True)
    if not project.is_dir():
        parser.error("--project must be a directory")
    work = repo / "target" / "dev-ui"
    home = (args.home or work / "home").expanduser().resolve()
    live_home = Path(os.environ.get("RIWORK_HOME") or Path.home() / ".local/share/riwork").expanduser().resolve()
    if home == live_home:
        parser.error("--home must be separate from the active RiWork data directory")
    if args.port is not None and not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    work.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(work / "cargo")
    env["NSH_CLI"] = str(nsh)
    env["RIWORK_HOME"] = str(home)
    env["RIWORK_RUNTIME_DIR"] = str(work / "runtime")
    env["RIWORK_NSH_BUNDLE"] = str(work / "ui" / "bundle.js")
    for key in ["RIWORK_CHAT_ID", "RIWORK_SHELL_ID", "RIWORK_RESTORE_TICKET", "RIWORK_NSH_DEV_URL"]:
        env.pop(key, None)
    run(["cargo", "build", "--locked", "-p", "riwork-desktop", "--features", "dev-reload"], cwd=repo, env=env)
    run([nsh, "build", "apps/riwork", "--out", work / "ui"], cwd=repo, env=env)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", args.port or 0))
        port = probe.getsockname()[1]
    server = desktop = None
    try:
        command = [str(nsh), "dev", "apps/riwork", "--server-only", "--port", str(port)]
        print("+ " + " ".join(command), flush=True)
        server = subprocess.Popen(command, cwd=repo, env=env, start_new_session=True)
        deadline = time.monotonic() + 60
        while True:
            if server.poll() is not None:
                raise RuntimeError("NSH dev server exited before it was ready")
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("NSH dev server did not become ready within 60 seconds")
                time.sleep(0.1)
        env["RIWORK_NSH_DEV_URL"] = f"ws://127.0.0.1:{port}/ws"
        print(f"Development data: {home}\nSave TSX to reload the UI. Component state resets; Rust edits require restarting this command.", flush=True)
        desktop = subprocess.Popen([str(work / "cargo/debug/riwork"), str(project)], cwd=repo, env=env, start_new_session=True)
        while desktop.poll() is None:
            if server.poll() is not None:
                raise RuntimeError("NSH dev server stopped")
            time.sleep(0.2)
        return desktop.returncode
    finally:
        stop(desktop)
        stop(server)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"dev-ui: {error}", file=sys.stderr)
        sys.exit(1)
