#!/usr/bin/env python3
"""Profile an isolated Debug UI with deterministic native transcript actions.

Seed tool-ui-fixture.py and run tool-ui-perf-host.py first. The binary must have
Debug assertions and RIWORK_TOOL_UI_PERF_CONTROL support. Never runs a provider.
"""
import argparse
import json
from pathlib import Path
import subprocess
import socket
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--home', type=Path, default=Path('/tmp/riwork-perf-home'))
    parser.add_argument('--output', type=Path, required=True, help='New output prefix under /tmp')
    args = parser.parse_args()
    home, output = args.home.resolve(), args.output.resolve()
    if not all(path.is_relative_to(Path('/tmp').resolve()) for path in (home, output)):
        parser.error('--home and --output must be under /tmp')
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(3)
        connection.connect(str(home/'run/chat.sock'))
        connection.sendall(b'{"id":"profile-probe","op":"fixture_status"}\n')
        status = json.loads(connection.makefile('rb').readline())
        if not (status.get('result') or {}).get('fixture'):
            parser.error('start tool-ui-perf-host.py before profiling')
    fixture = json.loads((home/'fixture.json').read_text())
    # Preview is a tab: the previous run may have made it active. Restore
    # only this disposable fixture's chat tab before starting the next run.
    layouts_path = home/'layouts.json'
    layouts = json.loads(layouts_path.read_text())
    layout = layouts['projects'][fixture['project']]
    layout.update(layout={'pane': 1}, active_pane=1, main_pane=1)
    layout['panes'] = {'1': dict(tabs=[dict(kind='chat', chat_id=fixture['chat'])],
                                active_tab_key='chat:'+fixture['chat'])}
    layouts_path.write_text(json.dumps(layouts, indent=2))
    trace, log, control = (Path(str(output)+suffix) for suffix in ('.trace', '.log', '.control'))
    if any(path.exists() for path in (trace, log, control)):
        parser.error('output already exists; choose a fresh prefix')
    command = ['xcrun', 'xctrace', 'record', '--template', 'Time Profiler',
               '--output', str(trace), '--time-limit', '180s', '--no-prompt']
    for value in (f'RIWORK_HOME={home}', f'RIWORK_RUNTIME_DIR={output}-runtime',
                  'TMPDIR=/tmp', 'ZED_MEASUREMENTS=1', 'RIWORK_TOOL_UI_PERF=1',
                  f'RIWORK_TOOL_UI_PERF_CONTROL={control}'):
        command += ['--env', value]
    command += ['--target-stdout', str(log), '--launch', '--', str(args.binary.resolve()), fixture['root']]
    with Path(str(output)+'-trace.log').open('w') as recorder_log:
        recorder = subprocess.Popen(command, stdout=recorder_log, stderr=recorder_log)
        serial = 0

        def action(text, settle=1):
            nonlocal serial
            serial += 1
            offset = log.stat().st_size if log.exists() else 0
            control.write_text(f'{serial} {text}')
            deadline = time.monotonic() + 60
            marker = f'tool-ui-perf command {text} '
            while time.monotonic() < deadline:
                if log.exists():
                    with log.open() as stream:
                        stream.seek(offset)
                        if marker in stream.read():
                            print(text, flush=True)
                            time.sleep(settle)
                            return
                if recorder.poll() is not None:
                    raise RuntimeError(f'profiler exited before {text}; see {output}-trace.log')
                time.sleep(.1)
            raise RuntimeError(f'no acknowledgement of {text}; Debug binary/isolated home required')

        time.sleep(6)
        action('normal')
        action('scroll:99999')
        action('stream-text', 0)
        host = Path(__file__).with_name('tool-ui-perf-host.py')
        subprocess.run([sys.executable, str(host), '--home', str(home), '--trigger',
                        '--tokens', '300', '--rate', '30', '--kind', 'text'], check=True)
        time.sleep(11)
        action('stream-output', 0)
        subprocess.run([sys.executable, str(host), '--home', str(home), '--trigger',
                        '--tokens', '300', '--rate', '30', '--kind', 'output'], check=True)
        time.sleep(11)
        action('verbose')
        action('collapse')
        action('scroll:0')
        action('expand', 3)
        action('scroll:10', 3)
        group = 'g:perf-dense-turn:perf-dense-read-00000'
        action('toggle:'+group, 2)
        action('toggle:'+group, 2)
        action('normal')
        action('verbose')
        action('normal')
        action('verbose')
        action('preview:perf-dense-read-00000', 3)
        action('quit', 0)
        recorder.wait(timeout=45)
        if recorder.returncode:
            raise RuntimeError(f'profiler exited {recorder.returncode}; see {output}-trace.log')
    print(trace)


if __name__ == '__main__':
    main()
