#!/usr/bin/env python3
"""Check the captured fixture run, without connecting to an app or tmux server.

Usage: verify_evidence.py EVIDENCE_DIRECTORY
Prints JSON. Core interaction checks are separate from required teardown.
Exit 1 means failed assertions or a confirmed lifecycle blocker; exit 2 means
required teardown remains unverified. This script never infers recovery.
"""
import argparse
import collections
import datetime
import json
import pathlib
import re
import sys


def read_jsonl(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def verify(root):
    inputs = read_jsonl(root / 'input-events.jsonl')
    native = read_jsonl(root / 'native-events.jsonl')
    initial = (root / 'session-initial.txt').read_text().strip().split()
    if len(initial) != 4:
        raise ValueError('session-initial.txt must contain session, creation time, pane, pane PID')
    snapshots = {}
    for filename in ['before-gui.txt', 'detached.txt', 'reattached.txt', 'after-gui.txt']:
        content = (root / filename).read_text()
        first = re.fullmatch(
            r'session=(\S+) created=(\d+) pane=(\S+) pane_pid=(\d+) clients=(\d+) size=(\d+)x(\d+)',
            content.splitlines()[0],
        )
        if not first:
            raise ValueError(f'{filename} does not have the expected identity header')
        snapshots[filename] = {
            'session': first[1], 'created': first[2], 'pane': first[3],
            'pane_pid': int(first[4]), 'clients': int(first[5]),
            'columns': int(first[6]), 'rows': int(first[7]),
            'client_pids': [int(pid) for pid in re.findall(r'^client=\S+ pid=(\d+)', content, re.MULTILINE)],
        }

    checks = {}

    def check(name, passed, **observed):
        checks[name] = {'status': 'PASS' if passed else 'FAIL', **observed}

    payload = b''.join(bytes.fromhex(row['hex']) for row in inputs if row['kind'] == 'input')
    markers = {'keyboard': 'NSH-keyboard-123', 'reattached': 'REATTACHED',
               'japanese_probe': '日本語', 'deadkey_composed': 'â'}
    counts = {name: payload.count(value.encode('utf-8')) for name, value in markers.items()}
    check('terminal_input_markers', all(counts.values()), counts=counts, input_bytes=len(payload))
    check('control_keys', b'\r' in payload and b'\x1b[D' in payload and b'\x03' in payload,
          carriage_return=b'\r' in payload, left_arrow=b'\x1b[D' in payload, ctrl_c=b'\x03' in payload)

    raw_mouse = [(int(m[1]), int(m[2]), int(m[3]), m[4].decode()) for m in
                 re.finditer(rb'\x1b\[<(\d+);(\d+);(\d+)([Mm])', payload)]
    mouse_counts = {
        'press': sum(code == 0 and state == 'M' for code, _x, _y, state in raw_mouse),
        'release': sum(code == 0 and state == 'm' for code, _x, _y, state in raw_mouse),
        'drag_code_32': sum(code == 32 for code, _x, _y, state in raw_mouse),
        'wheel_code_64': sum(code == 64 for code, _x, _y, state in raw_mouse),
    }
    check('mouse_raw_sgr', all(mouse_counts.values()), counts=mouse_counts)
    recorder_mouse = [(row['button'], row['column'], row['row'],
                       'm' if row['state'] == 'release' else 'M')
                      for row in inputs if row['kind'] == 'mouse']
    check('mouse_decoder_agrees_with_bytes', raw_mouse == recorder_mouse,
          raw_count=len(raw_mouse), decoded_count=len(recorder_mouse))

    sizes = [(row['columns'], row['rows']) for row in inputs if row['kind'] == 'resize']
    first_108 = sizes.index((108, 32)) if (108, 32) in sizes else -1
    later_small = [size for size in sizes[first_108 + 1:] if size in [(88, 24), (88, 25)]]
    check('tmux_resize_sequence', bool(sizes) and sizes[0] == (96, 26) and first_108 > 0 and bool(later_small),
          observed=[list(size) for size in sizes], expected='96x26 -> 108x32 -> 88x24 or 88x25')

    identities = [[s['session'], s['created'], s['pane'], str(s['pane_pid'])] for s in snapshots.values()]
    recorder_pids = sorted({row['recorder_pid'] for row in inputs})
    starts = [row for row in inputs if row['kind'] == 'start']
    check('session_pane_and_recorder_persist', all(identity == initial for identity in identities)
          and recorder_pids == [int(initial[3])] and len(starts) == 1 and starts[0]['pane'] == initial[2],
          initial=initial, recorder_pids=recorder_pids, recorder_start_count=len(starts), snapshots=snapshots)
    client_sets = [snapshots[name]['client_pids'] for name in ['detached.txt', 'reattached.txt', 'after-gui.txt']]
    check('tmux_client_survives_view_detach', bool(client_sets[0]) and all(pids == client_sets[0] for pids in client_sets),
          client_pids=client_sets)

    detaches = [row for row in native if row['event'] == 'detached']
    if not detaches:
        raise ValueError('No native detached event identifies the exercised app process')
    # Ignore the failed first command launch and any later explicit app relaunch.
    # The tested native-view lifecycle is identified by its own detach event.
    exercise_pid = detaches[0]['pid']
    exercised = [row for row in native if row['pid'] == exercise_pid]
    event_counts = collections.Counter(row['event'] for row in exercised)
    surfaces = sorted({row['surface'] for row in exercised})
    reattaches = [row for row in exercised if row['event'] == 'reattached']
    live_themes = [row for row in exercised if row['event'] == 'theme-updated'
                   and row['view_width'] > 0 and row['view_height'] > 0]
    check('native_surface_persists_through_theme_and_view_lifecycle',
          len(surfaces) == 1 and surfaces[0] not in ['0x0', '(nil)']
          and event_counts['created'] == 1 and bool(reattaches)
          and len(live_themes) >= 2 and len({row['theme_count'] for row in live_themes}) >= 2,
          app_pid=exercise_pid, surfaces=surfaces, event_counts=dict(event_counts),
          live_theme_counts=[row['theme_count'] for row in live_themes])
    check('native_detach_reattach_flags', bool(reattaches)
          and detaches[0]['detached'] and not reattaches[0]['detached']
          and detaches[0]['time'] < reattaches[0]['time']
          and reattaches[0]['detach_count'] >= 1 and reattaches[0]['attach_count'] >= 1)
    reattach_time = reattaches[0]['time'] if reattaches else float('inf')
    post_reattach = b''.join(bytes.fromhex(row['hex']) for row in inputs if row['kind'] == 'input'
                           and datetime.datetime.fromisoformat(row['utc']).timestamp() >= reattach_time)
    check('input_after_native_reattach', b'REATTACHED' in post_reattach)

    scales = [row for row in exercised if row['event'] == 'synthetic-scale-probe']
    scale_details = [{key: row[key] for key in ['forced_scale', 'backing_scale', 'view_width', 'view_height', 'width_px', 'height_px']}
                     for row in scales]
    check('synthetic_content_scale_dimensions', {row['forced_scale'] for row in scales} >= {1, 2}
          and all(row['view_width'] > 0 and row['view_height'] > 0
                  and abs(row['width_px'] - row['view_width'] * row['forced_scale']) <= 1
                  and abs(row['height_px'] - row['view_height'] * row['forced_scale']) <= 1 for row in scales),
          observations=scale_details, classification='Synthetic setter probe, not physical display migration')

    marked_key_sequences = 0
    for index, row in enumerate(exercised):
        following = exercised[index + 1:index + 4]
        if row['event'] == 'set-marked-text' and len(following) == 3:
            if ([next_row['event'] for next_row in following] == ['key-down', 'insert-text', 'key-down']
                    and following[0]['marked_length'] > 0 and following[-1]['marked_length'] == 0):
                marked_key_sequences += 1
    check('deadkey_marked_text_and_commit', counts['deadkey_composed'] > 0 and marked_key_sequences > 0,
          composed_utf8='â', committed_occurrences=counts['deadkey_composed'],
          marked_key_commit_sequences=marked_key_sequences,
          classification='GUI keyboard dead-key composition observed; not a Japanese input-source test')
    check('synthetic_japanese_marked_text_probe', counts['japanese_probe'] > 0 and event_counts['ime-probe-committed'] > 0,
          committed_utf8='日本語', classification='Synthetic NSTextInputClient callback probe; Japanese input-source GUI UNVERIFIED')

    inventory = {}
    for row in exercised:
        for screen in row.get('screens', []):
            inventory[str(screen['display_id'])] = screen
    limitations = {
        'physical_display_scale_change': {
            'status': 'UNVERIFIED',
            'reason': 'Only one physical display at backing scale 2 was available; 1x/2x setter probes are not display migration.',
            'observed_displays': list(inventory.values()),
        },
        'japanese_input_source_ime_gui': {
            'status': 'UNVERIFIED',
            'reason': 'Japanese bytes came from the synthetic IME callback probe; candidate UI and Japanese input-source routing were not tested.',
        },
        'theme_pixels': {
            'status': 'MANUAL_SCREENSHOT_REVIEW',
            'reason': 'This script validates theme callbacks and surface identity; screenshots establish the visible colors.',
        },
    }
    # A successful interaction run does not establish safe native-component
    # destruction. Match the captured main-thread wait to this exact app PID.
    teardown = {
        'status': 'UNVERIFIED',
        'reason': 'No matched teardown sample/process evidence establishes successful termination.',
        'recovery': 'Not inferred by this script; external cleanup cannot establish safe teardown.',
    }
    sample_path = root / 'teardown-sample.txt'
    processes_path = root / 'teardown-processes.txt'
    if sample_path.is_file() and processes_path.is_file():
        sample = sample_path.read_text()
        processes = processes_path.read_text()
        sample_pid = re.search(r'^Process:.*\[(\d+)\]', sample, re.MULTILINE)
        main_thread = re.search(r'^    \d+ Thread_[^\n]*com\.apple\.main-thread[^\n]*\n(.*?)(?=^    \d+ Thread_|\Z)',
                                sample, re.MULTILINE | re.DOTALL)
        stack = main_thread[1] if main_thread else ''
        primary_alive = re.search(rf'^\s*{exercise_pid}\s+.*riwork-ghostty-nsh-spike\s*$', processes, re.MULTILINE)
        stack_markers = ['spike_terminal_view_shutdown', 'ghostty_surface_free', '_pthread_join', '__ulock_wait']
        matched_blocker = (sample_pid is not None and int(sample_pid[1]) == exercise_pid
                           and primary_alive is not None and all(marker in stack for marker in stack_markers)
                           and event_counts['shutdown'] > 0)
        closed_path = root / 'closed.txt'
        closed = closed_path.read_text() if closed_path.is_file() else ''
        closed_client_pids = [int(pid) for pid in re.findall(r'^client=\S+ pid=(\d+)', closed, re.MULTILINE)]
        teardown.update({
            'sample_pid': int(sample_pid[1]) if sample_pid else None,
            'exercised_app_pid': exercise_pid,
            'primary_process_present_in_teardown_capture': primary_alive is not None,
            'matched_main_thread_frames': [marker for marker in stack_markers if marker in stack],
            'native_shutdown_event_count': event_counts['shutdown'],
            'closed_snapshot_client_pids': closed_client_pids,
            'artifacts': ['teardown-sample.txt', 'teardown-processes.txt', 'closed.txt'],
        })
        if matched_blocker:
            teardown['status'] = 'FAIL'
            teardown['reason'] = (
                'Captured window-close/native-component teardown blocks the main thread in '
                'ghostty_surface_free -> pthread_join -> ulock_wait; the matching fixture '
                'process remains present in the teardown process capture.'
            )

    failed = [name for name, result in checks.items() if result['status'] == 'FAIL']
    required_failures = failed + (['teardown_termination'] if teardown['status'] == 'FAIL' else [])
    return {
        'status': ('NO_GO_LIFECYCLE_BLOCKER' if teardown['status'] == 'FAIL' else
                   'CORE_INTERACTION_CHECKS_FAILED' if failed else 'CORE_INTERACTIONS_VERIFIED_TEARDOWN_UNVERIFIED'),
        'overall_readiness': 'NO_GO' if required_failures else 'UNVERIFIED',
        'core_interaction_status': 'FAIL' if failed else 'PASS_WITH_LIMITATIONS',
        'evidence_directory': str(root),
        'scope': 'The 13 checks cover core interaction only. Teardown is a separate required lifecycle gate. No desktop input or tmux commands are sent.',
        'failed_checks': failed,
        'required_failures': required_failures,
        'checks': checks,
        'teardown_termination': teardown,
        'limitations': limitations,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence_directory', type=pathlib.Path)
    args = parser.parse_args()
    try:
        result = verify(args.evidence_directory.resolve())
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        result = {'status': 'EVIDENCE_ERROR', 'overall_readiness': 'UNVERIFIED', 'error': str(error)}
    print(json.dumps(result, indent=2, ensure_ascii=False))
    if result['overall_readiness'] == 'NO_GO' or result['status'] == 'EVIDENCE_ERROR':
        return 1
    return 2 if result['teardown_termination']['status'] == 'UNVERIFIED' else 0


if __name__ == '__main__':
    sys.exit(main())
