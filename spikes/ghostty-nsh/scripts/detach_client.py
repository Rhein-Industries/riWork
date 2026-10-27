#!/usr/bin/env python3
"""Off-main-thread, exact-tty detach handshake for this disposable fixture."""
import json
import os
import pathlib
import subprocess
import sys
import time

home = pathlib.Path(sys.argv[1]).resolve()
tty, surface_id, fixture_pid = sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
assert (home / 'GHOSTTY_NSH_SPIKE_FIXTURE').is_file()
assert os.environ['SPIKE_TMUX_SOCKET'] == str(home / 'tmux.sock')
assert tty.startswith('/dev/ttys')
base = [os.environ['SPIKE_TMUX'], '-S', str(home / 'tmux.sock')]
deadline = time.monotonic() + 1.5
result = dict(tty=tty, surface_id=surface_id, fixture_pid=fixture_pid, detached=False)


def run(args):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError('detach deadline')
    return subprocess.check_output(args, text=True, timeout=min(0.4, remaining)).strip()


def clients():
    rows = run(base + ['list-clients', '-F', '#{client_pid}|#{client_tty}|#{session_id}'])
    return [dict(pid=int(pid), tty=name, session=session)
            for pid, name, session in (row.split('|') for row in rows.splitlines())]


def identity():
    return run(base + ['display-message', '-p', '-t', 'ghostty-spike',
                       '#{session_id} #{session_created} #{pane_id} #{pane_pid}'])


try:
    result['identity_before'] = identity()
    result['before'] = clients()
    matches = [c for c in result['before'] if c['tty'] == tty]
    # No detach-all or ambiguous fallback. A missing attach at release is a
    # failed pressure trial, even if freeing an already-exited child is safe.
    if len(matches) != 1:
        raise RuntimeError(f'expected one attached client for {tty}: {matches}')
    client_pid = result['client_pid'] = matches[0]['pid']
    wrapper_pid = result['wrapper_pid'] = int(run(['/bin/ps', '-p', str(client_pid), '-o', 'ppid=']))
    wrapper_parent = int(run(['/bin/ps', '-p', str(wrapper_pid), '-o', 'ppid=']))
    if wrapper_parent != fixture_pid:
        raise RuntimeError('client is not a login-wrapper descendant of this fixture')
    result['detach_command'] = base + ['detach-client', '-t', tty]
    run(result['detach_command'])
    while True:
        result['after'] = clients()
        if not any(c['tty'] == tty or c['pid'] == client_pid for c in result['after']):
            break
        if time.monotonic() >= deadline:
            raise TimeoutError('client still attached')
        time.sleep(0.01)
    result['identity_after'] = identity()
    if result['identity_before'] != result['identity_after']:
        raise RuntimeError('session/pane changed during detach')
    result['detached'] = True
except Exception as error:
    result['error'] = str(error)
finally:
    result['monotonic_ms'] = time.monotonic() * 1000
    path = home / f'detach-{surface_id}.json'
    path.write_text(json.dumps(result, indent=2) + '\n')
sys.exit(0 if result['detached'] else 1)
