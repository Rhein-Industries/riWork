#!/usr/bin/env python3
"""Read back one attached/closed batch. Never sends GUI or terminal input."""
import argparse
import json
import os
import pathlib
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('home', type=pathlib.Path)
parser.add_argument('fixture_pid', type=int)
parser.add_argument('cycle', type=int)
parser.add_argument('phase', choices=('attached', 'closed'))
parser.add_argument('--expect-exit', action='store_true')
args = parser.parse_args()
home = args.home.resolve()
assert (home / 'GHOSTTY_NSH_SPIKE_FIXTURE').is_file()
base = [os.environ['SPIKE_TMUX'], '-S', str(home / 'tmux.sock')]


def read(args):
    return subprocess.check_output(args, text=True, timeout=1).strip()


def observe():
    events = [json.loads(x) for x in (home / 'teardown-events.jsonl').read_text().splitlines()]
    ids = [e['surface_id'] for e in events if e['event'] == 'created'][-4:]
    clients = [dict(pid=int(pid), tty=tty, session=session) for pid, tty, session in
               (line.split('|') for line in read(base + ['list-clients', '-F',
                 '#{client_pid}|#{client_tty}|#{session_id}']).splitlines())]
    identity = read(base + ['display-message', '-p', '-t', 'ghostty-spike',
                           '#{session_id} #{session_created} #{pane_id} #{pane_pid}'])
    processes = {}
    for line in read(['/bin/ps', '-axo', 'pid=,ppid=,stat=,command=']).splitlines():
        pid, ppid, stat, command = line.strip().split(None, 3)
        processes[int(pid)] = dict(pid=int(pid), ppid=int(ppid), stat=stat, command=command)
    descendants = []
    def descend(pid):
        for row in processes.values():
            if row['ppid'] == pid:
                descendants.append(row)
                descend(row['pid'])
    descend(args.fixture_pid)
    helpers = [json.loads((home / f'detach-{sid}.json').read_text())
               for sid in ids if (home / f'detach-{sid}.json').exists()]
    old = {h[key] for h in helpers for key in ('client_pid', 'wrapper_pid') if key in h}
    pressure = json.loads((home / 'pressure.json').read_text())
    disposed = {e['surface_id'] for e in events if e['event'] == 'disposed'}
    return dict(cycle=args.cycle, phase=args.phase, surface_ids=ids,
                fixture_pid=args.fixture_pid, monotonic_ms=time.monotonic()*1000,
                clients=clients, identity=identity, descendants=descendants,
                alive_pids=sorted(old & processes.keys()), pressure_bytes=pressure['bytes'],
                pressure_sequence=pressure['sequence'], pressure=pressure,
                all_disposed=set(ids) <= disposed, fixture_alive=args.fixture_pid in processes)


deadline = time.monotonic() + 2.5
while True:
    row = observe()
    ready = (not row['fixture_alive'] if args.expect_exit else row['fixture_alive']) and len(row['surface_ids']) == 4
    if args.phase == 'attached':
        ready = ready and len(row['clients']) == 4 and len(row['descendants']) == 8
    else:
        ready = ready and row['all_disposed'] and not row['clients'] and not row['descendants'] and not row['alive_pids']
    if ready or time.monotonic() >= deadline:
        break
    time.sleep(0.02)
row['ready'] = ready
with (home / 'cycles.jsonl').open('a') as log:
    log.write(json.dumps(row) + '\n')
print(json.dumps({key: row[key] for key in ('cycle', 'phase', 'surface_ids', 'ready', 'identity', 'pressure_bytes', 'alive_pids')}))
raise SystemExit(0 if ready else 1)
