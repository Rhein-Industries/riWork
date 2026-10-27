#!/usr/bin/env python3
"""Launch only the isolated, privately linked pinned fixture and record its exit.

Source the prepared fixture.env first. This performs no desktop input; all GUI
actions are through RiWork's Cua Driver with the resulting fixture PID.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


spike = pathlib.Path(__file__).resolve().parents[1]
home = pathlib.Path(os.environ['RIWORK_HOME']).resolve()
assert (home / 'GHOSTTY_NSH_SPIKE_FIXTURE').is_file()
assert os.environ['SPIKE_TMUX_SOCKET'] == str(home / 'tmux.sock')
assert os.environ['SPIKE_SURFACES'] == '4'
fixture = spike / '.runtime/pinned/fixture'
archive = spike / '.runtime/pinned/install/lib/libghostty-internal.a'
header = spike / '.runtime/pinned/install/include/ghostty.h'
pin = json.loads((spike / 'evidence/phase2/runtime/pinned-rebuild-provenance.json').read_text())
assert digest(archive) == pin['lib/libghostty-internal.a_sha256']
assert digest(header) == pin['include/ghostty.h_sha256']
runtime = dict(runtime_label='pinned-9f0e171-profiled',
               ghostty_upstream_revision=pin['ghostty_upstream_revision'],
               archive_path=str(archive), archive_sha256=digest(archive),
               header_path=str(header), header_sha256=digest(header),
               fixture_path=str(fixture), fixture_sha256=digest(fixture),
               RIWORK_HOME=str(home))
(home / 'runtime.json').write_text(json.dumps(runtime, indent=2) + '\n')
with (home / 'fixture.log').open('w') as output:
    child = subprocess.Popen([str(fixture)], stdout=output, stderr=subprocess.STDOUT)
    launched = dict(fixture_pid=child.pid, fixture_path=str(fixture),
                    fixture_sha256=runtime['fixture_sha256'],
                    started_at_unix=time.time(), monotonic_ms=time.monotonic()*1000)
    (home / 'launch.json').write_text(json.dumps(launched, indent=2) + '\n')
    print(json.dumps(launched), flush=True)
    result = child.wait()
exit_record = dict(fixture_pid=child.pid, exit_code=result,
                   exited_at_unix=time.time(), monotonic_ms=time.monotonic()*1000,
                   profile_flushed=(home / 'main-profile.jsonl').is_file(),
                   host_stopped='SPIKE host stopped' in (home / 'fixture.log').read_text())
(home / 'launcher-exit.json').write_text(json.dumps(exit_record, indent=2) + '\n')
print(json.dumps(exit_record), flush=True)
raise SystemExit(result)
