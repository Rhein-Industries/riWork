#!/usr/bin/env python3
"""Real Swift/Rust/installed RiWork interop in a disposable RIWORK_HOME.
Never reads or mutates the user's RiWork registry or existing sessions.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--relay-binary', required=True, type=Path)
parser.add_argument('--riwork', required=True, type=Path)
parser.add_argument('--smoke-binary', type=Path, default=Path(__file__).resolve().parents[1] / '.build/debug/riwork-ios-smoke')
parser.add_argument('--hold', type=int, default=0, help='Keep the fixture available for simulator pairing for this many seconds')
parser.add_argument('--viewport', action='store_true', help='Require the published resize extension; verify real PTY cells and desktop restoration')
parser.add_argument('--protocol', type=int, default=1, choices=(1, 2), help='1 is the long-lived PSK. 2 is a single-use invite.')
args = parser.parse_args()
finish_hold = threading.Event()
signal.signal(signal.SIGUSR1, lambda *_: finish_hold.set())
root = Path(tempfile.mkdtemp(prefix='riwork-swift-interop-'))
root.chmod(0o700)
home = root / 'home'
home.mkdir(mode=0o700)
project_dir = root / 'project'
project_dir.mkdir()
# Only child processes use the fixture home; the inherited environment is retained.
env = os.environ.copy()
env['RIWORK_HOME'] = str(home)
children = []
created_shells = []
logs = []

def run(command, check=True):
    result = subprocess.run([str(x) for x in command], env=env, capture_output=True, text=True, timeout=30)
    if check and result.returncode:
        raise RuntimeError(f'{Path(str(command[0])).name} {command[1]} failed: {result.stderr}')
    return result

def cli(*argv):
    return json.loads(run([args.riwork, *argv, '--json']).stdout)

def start(command, log_name):
    log = (root / log_name).open('w')
    logs.append(log)
    child = subprocess.Popen([str(x) for x in command], env=env, stdout=log, stderr=log)
    children.append(child)
    return child

def wait_health(port):
    for _ in range(100):
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=0.5) as response:
                if response.status == 200:
                    return
        except Exception:
            time.sleep(0.05)
    raise RuntimeError('Relay did not become healthy')

def pane(shell_id):
    # Same documented FNV-1a socket selection as SessionManager, only our temp home.
    digest = 0xcbf29ce484222325
    for byte in str(home.resolve()).encode():
        digest = ((digest ^ byte) * 0x100000001b3) & 0xffffffffffffffff
    tmux = shutil.which('tmux') or '/opt/homebrew/bin/tmux'
    data = run([tmux, '-L', f'riwork-{digest:016x}', 'list-panes', '-t', shell_id, '-F', '#{pane_id} #{pane_pid} #{pane_width} #{pane_height}']).stdout.strip().splitlines()
    if len(data) != 1: raise RuntimeError('Fixture must remain one unsplit pane')
    return data[0].split()

try:
    run(['git', '-C', project_dir, 'init'])
    run(['git', '-C', project_dir, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@localhost', 'commit', '--allow-empty', '-m', 'Isolated fixture'])
    project = cli('project', 'add', project_dir, '--name', 'Swift interop fixture')
    project_id = project['id']
    cli('task', 'add', 'Continue isolated Swift fixture', '--project', project_id)
    shell = cli('shell', 'create', '--project', project_id, '--command', '/bin/zsh')
    shell_id = shell['id']
    created_shells.append(shell_id)
    orchestrator = cli('orchestrator', 'create', '--project', project_id, '--command', '/bin/zsh')
    created_shells.append(orchestrator['id'])
    run([args.riwork, 'shell', 'send', shell_id, "printf 'EXISTING_SWIFT_FIXTURE\\n'"])
    time.sleep(0.2)
    baseline = pane(shell_id) if args.viewport else None
    with socket.socket() as available:
        available.bind(('127.0.0.1', 0))
        port = available.getsockname()[1]
    pairing = root / 'device.pairing.json'
    routes = root / 'routes.json'
    pair = [args.relay_binary, 'pair', '--relay', f'ws://127.0.0.1:{port}/v1/ws', '--name', 'Swift fixture', '--out', pairing, '--relay-routes', routes, '--allow-insecure-loopback']
    if args.protocol == 2:
        pair += ['--protocol', '2', '--ttl-seconds', '600']
    run(pair)
    start([args.relay_binary, 'relay', '--bind', f'127.0.0.1:{port}', '--routes', routes], 'relay.log')
    wait_health(port)
    start([args.relay_binary, 'start', '--riwork', args.riwork], 'connector.log')
    time.sleep(0.3)
    marker = root / 'input-count'
    size_file = root / 'pty-size'
    line = f"printf x >> {shlex.quote(str(marker))}; printf 'SWIFT_CONTINUED\\n'"
    if args.viewport: line = f"stty size > {shlex.quote(str(size_file))}; " + line
    smoke_command = [args.smoke_binary, pairing, '--local', '--project', project_id, '--shell', shell_id, '--send', line]
    if args.viewport: smoke_command += ['--columns', '43', '--rows', '17']
    established = root / 'established.pairing.json'
    if args.protocol == 2:
        smoke_command += ['--write-established', established]
    smoke = run(smoke_command)
    print(smoke.stdout, end='', flush=True)
    for _ in range(80):
        if marker.exists() and marker.read_bytes() == b'x': break
        time.sleep(0.05)
    if not marker.exists() or marker.read_bytes() != b'x':
        raise RuntimeError('Submission was missing or duplicated after reconnect')
    if args.viewport:
        if not size_file.exists() or size_file.read_text().strip() != '17 43':
            raise RuntimeError('Real shell PTY did not receive the mobile cell dimensions')
        for _ in range(100):
            current_pane = pane(shell_id)
            if current_pane == baseline: break
            time.sleep(0.1)
        if current_pane != baseline: raise RuntimeError(f'Desktop dimensions/pane/PID not restored: {baseline} -> {current_pane}')
        print(f'PASS: real PTY 43x17; desktop restored to {baseline[2]}x{baseline[3]}; same pane {baseline[0]} and PID {baseline[1]}', flush=True)
    current = cli('shell', 'output', shell_id, '--lines', '100')
    if 'SWIFT_CONTINUED' not in json.dumps(current): raise RuntimeError('Continuation missing in real tmux output')
    alive = cli('shell', 'list', '--project', project_id)
    if not any(s['id'] == shell_id and s['alive'] for s in alive): raise RuntimeError('Existing session did not survive reconnect')
    print('PASS: exactly one executed line; same persistent session alive after Swift reconnect', flush=True)
    if args.protocol == 2:
        # The first process keeps the established root in memory and writes it aside.
        # The original file is still the invite, so a second process must be rejected.
        replay = run([args.smoke_binary, pairing, '--local'], check=False)
        if replay.returncode == 0:
            raise RuntimeError('A second process accepted an already consumed v2 invite')
        print('PASS: consumed v2 invite is rejected for a second process', flush=True)
        saved = json.loads(established.read_text())
        if saved.get('v') != 2 or saved.get('invite_state') != 'established' or not saved.get('root_key') or saved.get('invite_secret'):
            raise RuntimeError('Established pairing file is missing the root or still holds the invite secret')
        resume = run([args.smoke_binary, established, '--local', '--project', project_id, '--shell', shell_id])
        print(resume.stdout, end='', flush=True)
        if marker.read_bytes() != b'x':
            raise RuntimeError('Persisted-root reconnect executed the fixture line again')
        print('PASS: a new process continued with the stored root and did not replay the invite', flush=True)
    # These are fixture-only paths/IDs, never secret contents.
    metadata = {'root': str(root), 'home': str(home), 'pairing_file': str(pairing), 'project_id': project_id, 'shell_id': shell_id, 'orchestrator_id': orchestrator['id'], 'port': port, 'baseline_pane': baseline}
    (root / 'fixture.json').write_text(json.dumps(metadata))
    print('Fixture metadata: ' + str(root / 'fixture.json'), flush=True)
    if args.hold:
        print(f'Fixture held for simulator inspection (PID {os.getpid()}); SIGUSR1 finishes verification/revocation early; Ctrl-C cleans up only fixture sessions.', flush=True)
        finish_hold.wait(args.hold)
    device_id = json.loads(pairing.read_text())['device_id']
    run([args.relay_binary, 'revoke', device_id])
    time.sleep(1.2)
    # v2 must present the stored root. The invite file is already rejected, so it cannot show that revoke closed the session.
    revoked_file = established if args.protocol == 2 else pairing
    denied = run([args.smoke_binary, revoked_file, '--local'], check=False)
    if denied.returncode == 0: raise RuntimeError('Revoked device connected')
    print('PASS: revoked device cannot authenticate/reconnect', flush=True)
finally:
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
            try: child.wait(timeout=3)
            except subprocess.TimeoutExpired: child.kill(); child.wait()
    for shell_id in created_shells:
        run([args.riwork, 'shell', 'close', shell_id], check=False)
    for log in logs: log.close()
    # Retain the protected directory for local evidence only. Pairing is disposable.
    print('Fixture processes and sessions cleaned up. Evidence directory: ' + str(root), flush=True)
