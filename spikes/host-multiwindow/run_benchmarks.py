#!/usr/bin/env python3
"""Launch only this spike's executable, with a new isolated data directory per run."""
import argparse
import json
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--repetitions', type=int, default=3)
parser.add_argument('--rows', type=int, nargs='+', default=[0, 2400])
parser.add_argument('--intervals', type=int, nargs='+', default=[0, 33])
args = parser.parse_args()
exe = ROOT / 'host/target/release/riwork-nsh-multiwindow-fixture'
bundle = ROOT / 'app/.nsh/out/bundle.js'
results = []
for rows in args.rows:
    for interval in args.intervals:
        for repeat in range(1, args.repetitions + 1):
            name = f'rows{rows}-interval{interval}-run{repeat}'
            log = ROOT / f'evidence/{name}.jsonl'
            with tempfile.TemporaryDirectory(prefix='nsh-isolated-') as data:
                command = [str(exe), '--bundle', str(bundle), '--data-dir', data,
                           '--output', str(log), '--history-rows', str(rows),
                           '--interval-ms', str(interval), '--timeout-ms', '120000']
                print('RUN', name, flush=True)
                with (ROOT / f'evidence/{name}.stderr').open('w') as stderr:
                    subprocess.run(command, check=True, stderr=stderr, timeout=135)
            events = [json.loads(line) for line in log.read_text().splitlines()]
            assert events[-1]['kind'] == 'exit' and events[-1]['success'], name
            windows = [e for e in events if e['kind'] == 'window']
            assert len(windows) == 2
            assert len({w['nativeWindowNumber'] for w in windows}) == 2
            for i, window_id in enumerate(['A', 'B']):
                done = [e for e in events if e['kind'] == 'hostEvent' and e['topic'] == 'done' and e['index'] == i]
                assert len(done) == 1, (name, i, done)
                payload = done[0]['payload']
                assert (payload['bytes'], payload['chunkCount'], payload['checksum']) == (18980, 475, '4f4771e9'), payload
                assert payload['windowId'] == window_id
                assert payload['nativeIdentity'] == f'riwork/native:0.3.0:{window_id}'
                assert payload['streamedMarkdownReads'] > 100, payload
                assert payload['streamedLayoutCount'] > 0 and payload['streamedHeight'] > 1000, payload
                perf = next(e['perf'] for e in events if e['kind'] == 'perfSettled' and e['index'] == i)
                assert perf['retainedFrameCount'] > 0, 'No instrumented surface frames'
                results.append({'run':name, 'rows':rows, 'intervalMs':interval, 'repeat':repeat,
                                'windowId':window_id, 'done':payload, 'perf':perf})
(ROOT / 'evidence/benchmark-summary.json').write_text(json.dumps(results, indent=2) + '\n')
print('Verified', len(results), 'window streams; summary: evidence/benchmark-summary.json')
