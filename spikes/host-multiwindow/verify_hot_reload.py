#!/usr/bin/env python3
"""Observe two same-process windows reload after an isolated TS source edit."""
import json
import os
import pathlib
import socket
import subprocess
import time
import tempfile
import fcntl

ROOT = pathlib.Path(__file__).resolve().parent
(ROOT / '.run').mkdir(exist_ok=True)
lock = (ROOT / '.run/hot-reload.lock').open('w')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
nsh = ROOT / '.tooling/target/release/nsh'
host = ROOT / 'host/target/release/riwork-nsh-multiwindow-fixture'
revision = ROOT / 'app/Revision.ts'
original = revision.read_text()
assert 'revision-1' in original
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
env = dict(os.environ, NSH_CACHE=str(ROOT / '.tooling/cache'))
log = ROOT / 'evidence/hot-reload.jsonl'
log.unlink(missing_ok=True)

def events():
    if not log.exists():
        return []
    result = []
    for line in log.read_text().splitlines():
        try:
            result.append(json.loads(line))
        except json.JSONDecodeError:  # writer may be between write and flush
            pass
    return result

def wait_for_revision(value):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        done = [e for e in events() if e.get('topic') == 'done' and e['payload']['revision'] == value]
        if {e['payload']['windowId'] for e in done} == {'A', 'B'}:
            for e in done:
                p = e['payload']
                assert (p['bytes'],p['chunkCount'],p['checksum']) == (18980,475,'4f4771e9')
            return
        if process.poll() is not None:
            raise RuntimeError(f'Host exited before both windows completed {value}')
        if server.poll() is not None:
            raise RuntimeError('nsh dev exited; see evidence/dev-server.log')
        time.sleep(0.1)
    raise TimeoutError(value)

def stop(child):
    if child is None or child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=10)

with tempfile.TemporaryDirectory(prefix='nsh-hmr-isolated-') as data, \
        (ROOT / 'evidence/dev-server.log').open('w') as devlog, \
        (ROOT / 'evidence/hot-reload.stderr').open('w') as hostlog:
    # --server-only conflicts with --target. The only connected host is our AppKit executable.
    server = subprocess.Popen([str(nsh),'dev',str(ROOT/'app'),'--server-only','--port',str(port)],
                              cwd=ROOT, env=env, stdout=devlog, stderr=subprocess.STDOUT)
    process = None
    try:
        process = subprocess.Popen([str(host),'--dev-url',f'ws://127.0.0.1:{port}/ws',
            '--data-dir',data,'--output',str(log),'--history-rows','0','--interval-ms','0',
            '--timeout-ms','130000','--exit-after-reloads','2'],
            cwd=ROOT, stdout=hostlog,stderr=subprocess.STDOUT)
        wait_for_revision('revision-1')
        print('Both windows completed revision-1; editing the isolated module', flush=True)
        revision.write_text(original.replace('revision-1','revision-2'))
        wait_for_revision('revision-2')
        print('Both windows completed revision-2 via nsh dev; waiting for fixture exit', flush=True)
        assert process.wait(timeout=75) == 0
        observed = events()
        assert len([e for e in observed if e['kind']=='devReload']) >= 2
        assert observed[-1]['success']
    finally:
        try:
            try:
                stop(process)
            finally:
                stop(server)
        finally:
            revision.write_text(original)
print('Verified automatic full remount in A and B; original module restored')
