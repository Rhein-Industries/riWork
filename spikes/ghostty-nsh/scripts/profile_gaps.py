#!/usr/bin/env python3
"""Attribute native timer gaps from the fixture's allocation-free timing trace.

This is a conservative observational test, not a subtraction-based relaxation of
the 100 ms metric. A long gap is accepted only when positively observed work
unrelated to teardown independently occupies at least 100 ms of that gap,
the union of main-thread teardown remains below 100 ms, and the unattributed
remainder remains below 100 ms. Short native frees and off-main detach waits
alone never establish that conclusion.
"""
import argparse
import collections
import gzip
import json
import math
import pathlib
import sys


BOUND_MS = 100.0
TEARDOWN_NAMES = frozenset((
    'shutdown-dispatch', 'closing-tick', 'closing-app-tick', 'child-exit-check',
    'surface-free', 'app-config-free',
))
UNRELATED_NAMES = frozenset(('runloop-idle-wait', 'create-surface', 'attached-tick',
                             'attached-app-tick'))
REQUIRED_SINGLE_SPANS = frozenset(('create-surface', 'shutdown-dispatch',
                                    'surface-free', 'app-config-free', 'detach-confirm-wait'))


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def read_jsonl(path):
    opener = gzip.open if path.suffix == '.gz' else open
    with opener(path, 'rt') as stream:
        rows = [json.loads(line) for line in stream if line.strip()]
    if not all(isinstance(row, dict) for row in rows):
        raise ValueError(f'{path.name} has a non-object record')
    return rows


def union(intervals):
    result = []
    for start, end in sorted(intervals):
        if end <= start:
            continue
        if result and start <= result[-1][1]:
            result[-1][1] = max(result[-1][1], end)
        else:
            result.append([start, end])
    return result


def subtract(intervals, excluded):
    result = []
    for start, end in union(intervals):
        cursor = start
        for left, right in union(excluded):
            if right <= cursor or left >= end:
                continue
            if left > cursor:
                result.append([cursor, min(left, end)])
            cursor = max(cursor, right)
            if cursor >= end:
                break
        if cursor < end:
            result.append([cursor, end])
    return result


def duration(intervals):
    return sum(end - start for start, end in union(intervals))


def analyze_trace(rows, native_events, fixture_pid=None):
    errors = []
    metadata = rows[0] if rows and rows[0].get('kind') == 'metadata' else {}
    events = rows[1:] if metadata else rows
    if not metadata:
        errors.append('missing first metadata record')
    if metadata.get('clock') != 'CLOCK_MONOTONIC':
        errors.append('trace is not declared CLOCK_MONOTONIC')
    if metadata.get('recorded') != len(events) or metadata.get('dropped') != 0:
        errors.append('trace count mismatch or dropped records')
    if (not isinstance(metadata.get('recorded'), int) or isinstance(metadata.get('recorded'), bool)
            or not isinstance(metadata.get('capacity'), int) or isinstance(metadata.get('capacity'), bool)
            or metadata.get('recorded', 0) > metadata.get('capacity', 0)):
        errors.append('invalid trace capacity/count declaration')
    if fixture_pid is not None and metadata.get('fixture_pid') != fixture_pid:
        errors.append('trace fixture PID mismatch')
    stacks = collections.defaultdict(list)
    prior_time = {}
    spans = []
    main_events = []
    for index, event in enumerate(events):
        valid = (event.get('seq') == index and number(event.get('monotonic_ms'))
                 and event['monotonic_ms'] >= 0 and isinstance(event.get('name'), str)
                 and event['name'] and event.get('phase') in ('b', 'e', 'i')
                 and isinstance(event.get('thread_id'), int) and not isinstance(event['thread_id'], bool)
                 and event['thread_id'] > 0 and isinstance(event.get('main_thread'), bool)
                 and isinstance(event.get('surface_id'), int) and not isinstance(event['surface_id'], bool)
                 and event['surface_id'] >= 0)
        if not valid:
            errors.append(f'invalid trace record at index {index}')
            continue
        tid, timestamp = event['thread_id'], event['monotonic_ms']
        if timestamp < prior_time.get(tid, timestamp):
            errors.append(f'thread {tid} timestamps go backwards at seq {index}')
        prior_time[tid] = timestamp
        if event['main_thread']:
            main_events.append(event)
        if event['phase'] == 'b':
            stacks[tid].append(event)
        elif event['phase'] == 'e':
            if not stacks[tid]:
                errors.append(f'unmatched span end at seq {index}')
                continue
            begin = stacks[tid].pop()
            if any(begin[key] != event[key] for key in ('name', 'surface_id', 'main_thread')):
                errors.append(f'mismatched/nonnested span at seq {index}')
                continue
            spans.append(dict(name=event['name'], surface_id=event['surface_id'],
                              thread_id=tid, main_thread=event['main_thread'],
                              start_ms=begin['monotonic_ms'], end_ms=timestamp,
                              begin_seq=begin['seq'], end_seq=index))
    for tid, stack in stacks.items():
        if stack:
            errors.append(f'thread {tid} has {len(stack)} unclosed spans')
    if len({event['thread_id'] for event in main_events}) != 1:
        errors.append('trace does not identify exactly one main thread')
    starts = [event for event in main_events if event['name'] == 'profile-start']
    exits = [event for event in main_events if event['name'] == 'process-exit']
    if len(starts) != 1 or len(exits) != 1:
        errors.append('missing or duplicate profile-start/process-exit')
    elif not events or events[0] != starts[0] or events[-1] != exits[0]:
        errors.append('trace is not bounded by profile-start/process-exit')

    # An idle proof is a directly adjacent pair on the MAIN thread. Nested
    # loops or any main work between before/after waiting cannot be idle proof.
    pending_wait = None
    loop_depth = 0
    boundary_count = 0
    leading_partial_loop = None
    loop_entries = loop_exits = 0
    for event in main_events:
        name = event['name']
        if pending_wait is not None:
            if name == 'runloop-after-waiting':
                spans.append(dict(name='runloop-idle-wait', surface_id=0,
                                  thread_id=event['thread_id'], main_thread=True,
                                  start_ms=pending_wait['monotonic_ms'], end_ms=event['monotonic_ms'],
                                  begin_seq=pending_wait['seq'], end_seq=event['seq']))
                pending_wait = None
                continue
            errors.append(f'main work interrupts run-loop wait at seq {event["seq"]}')
            pending_wait = None
        if name == 'runloop-before-waiting':
            pending_wait = event
        elif name == 'runloop-after-waiting':
            errors.append(f'unpaired run-loop wake at seq {event["seq"]}')
        elif name == 'runloop-entry':
            loop_depth += 1
            loop_entries += 1
            boundary_count += 1
        elif name == 'runloop-exit':
            loop_exits += 1
            if loop_depth > 0:
                loop_depth -= 1
            elif boundary_count == 0:
                # profileStart can run inside an existing NSH/AppKit source.
                # Its enclosing loop's entry predates observer registration.
                # Permit ONLY this first boundary to be an unmatched exit;
                # it supplies no inferred idle duration or other causal proof.
                leading_partial_loop = dict(exit_seq=event['seq'],
                                            exit_ms=event['monotonic_ms'],
                                            entry_observed=False)
            else:
                errors.append(f'unpaired run-loop exit at seq {event["seq"]}')
            boundary_count += 1
    if pending_wait:
        errors.append('unclosed run-loop wait')
    if loop_depth != 0:
        errors.append(f'unbalanced run-loop nesting: {loop_depth}')

    surface_ids = {str(event['surface_id']) for event in native_events if event.get('event') == 'created'}
    span_counts = collections.Counter((str(span['surface_id']), span['name']) for span in spans)
    for sid in sorted(surface_ids):
        for name in sorted(REQUIRED_SINGLE_SPANS):
            if span_counts[(sid, name)] != 1:
                errors.append(f'surface {sid} requires exactly one {name} span')
        for name in ('closing-tick', 'child-exit-check'):
            if span_counts[(sid, name)] < 1:
                errors.append(f'surface {sid} has no {name} span')
    for span in spans:
        if span['name'] in REQUIRED_SINGLE_SPANS | TEARDOWN_NAMES | UNRELATED_NAMES:
            expected_main = span['name'] != 'detach-confirm-wait'
            if span['main_thread'] != expected_main:
                errors.append(f'{span["name"]} has wrong thread affinity at seq {span["begin_seq"]}')
        if span['name'] != 'runloop-idle-wait' and str(span['surface_id']) not in surface_ids:
            errors.append(f'span has unknown surface {span["surface_id"]} at seq {span["begin_seq"]}')

    gaps = []
    raw_surface_gaps = []
    for index, event in enumerate(native_events):
        if event.get('event') != 'main-loop-gap':
            continue
        start, end, value = (event.get(key) for key in ('gap_start_ms', 'gap_end_ms', 'gap_ms'))
        if not all(number(item) for item in (start, end, value)) or start > end or abs((end - start) - value) > .01:
            errors.append(f'invalid exact surface gap endpoints at native record {index}')
            continue
        if len(starts) == 1 and len(exits) == 1 and not (starts[0]['monotonic_ms'] <= start <= end <= exits[0]['monotonic_ms']):
            errors.append(f'surface gap outside trace at native record {index}')
        raw_surface_gaps.append(value)
        if value >= BOUND_MS:
            gaps.append(dict(source='surface-timer', surface_id=event['surface_id'],
                             native_record=index, start_ms=start, end_ms=end, gap_ms=value))
    heartbeats = [event for event in main_events if event['name'] == 'heartbeat']
    if len(heartbeats) < 2:
        errors.append('fewer than two independent main-loop heartbeats')
    boundaries = starts[:1] + heartbeats + exits[:1]
    heartbeat_gaps = []
    for first, last in zip(boundaries, boundaries[1:]):
        gap = last['monotonic_ms'] - first['monotonic_ms']
        heartbeat_gaps.append(gap)
        if gap >= BOUND_MS:
            gaps.append(dict(source='global-heartbeat', surface_id=0,
                             begin_seq=first['seq'], end_seq=last['seq'],
                             begin_name=first['name'], end_name=last['name'],
                             start_ms=first['monotonic_ms'], end_ms=last['monotonic_ms'], gap_ms=gap))
    for gap in gaps:
        overlaps = []
        for span in spans:
            start, end = max(gap['start_ms'], span['start_ms']), min(gap['end_ms'], span['end_ms'])
            if end > start:
                overlaps.append({**span, 'overlap_start_ms': start, 'overlap_end_ms': end,
                                 'overlap_ms': end - start})
        main = [span for span in overlaps if span['main_thread']]
        teardown = [[span['overlap_start_ms'], span['overlap_end_ms']] for span in main
                    if span['name'] in TEARDOWN_NAMES]
        unrelated_busy = [[span['overlap_start_ms'], span['overlap_end_ms']] for span in main
                          if span['name'] in UNRELATED_NAMES and span['name'] != 'runloop-idle-wait']
        idle = [[span['overlap_start_ms'], span['overlap_end_ms']] for span in main
                if span['name'] == 'runloop-idle-wait']
        worker_wait = [[span['overlap_start_ms'], span['overlap_end_ms']] for span in overlaps
                       if span['name'] == 'detach-confirm-wait' and not span['main_thread']]
        # Idle observation excludes main-thread teardown execution but cannot
        # exclude indirect scheduling contention from the concurrent helpers.
        # Such overlap contributes no positive non-teardown evidence.
        positive_idle = subtract(idle, teardown + worker_wait)
        positive = union(subtract(unrelated_busy, teardown) + positive_idle)
        positive_ms = duration(positive)
        teardown_ms = duration(teardown)
        known_ms = duration(teardown + positive)
        unattributed_ms = max(0., gap['gap_ms'] - known_ms)
        accepted = (positive_ms >= BOUND_MS and teardown_ms < BOUND_MS
                    and unattributed_ms < BOUND_MS)
        cause = ('unrelated-run-loop-work-or-wait' if accepted
                 else 'surface-free' if any(span['name'] == 'surface-free' for span in main)
                 else 'teardown-or-mixed' if teardown_ms > 0 else 'unknown')
        gap.update(classification=cause, accepted_non_teardown=accepted,
                   positive_non_teardown_ms=positive_ms, teardown_main_union_ms=teardown_ms,
                   offmain_detach_wait_union_ms=duration(worker_wait),
                   idle_ms_excluded_for_detach_overlap=duration(subtract(idle, teardown)) - duration(positive_idle),
                   unattributed_ms=unattributed_ms,
                   rejection_reasons=([reason for condition, reason in (
                       (positive_ms < BOUND_MS, 'positive non-teardown occupancy below 100 ms'),
                       (teardown_ms >= BOUND_MS, 'main-thread teardown union at least 100 ms'),
                       (unattributed_ms >= BOUND_MS, 'unattributed remainder at least 100 ms'),
                   ) if condition]),
                   positive_non_teardown_intervals=positive,
                   overlap_union_ms_by_name={name: duration([[span['overlap_start_ms'], span['overlap_end_ms']]
                                                            for span in overlaps if span['name'] == name])
                                             for name in sorted({span['name'] for span in overlaps})},
                   overlaps=overlaps)
    valid = not errors
    raw_pass = valid and not gaps
    accepted = valid and all(gap['accepted_non_teardown'] for gap in gaps)
    return dict(trace_valid=valid, errors=errors, metadata=metadata, spans=len(spans),
                runloop_observation=dict(leading_partial_loop=leading_partial_loop,
                                         observed_entries=loop_entries, observed_exits=loop_exits,
                                         final_nesting_depth=loop_depth),
                raw_all_gaps_under_100ms=raw_pass, attribution_gate_passed=accepted,
                surface_timer_samples=len(raw_surface_gaps), heartbeat_samples=len(heartbeats),
                gap_count=len(gaps), surface_gap_count=sum(gap['source'] == 'surface-timer' for gap in gaps),
                heartbeat_gap_count=sum(gap['source'] == 'global-heartbeat' for gap in gaps),
                max_surface_gap_ms=max(raw_surface_gaps, default=None),
                max_heartbeat_gap_ms=max(heartbeat_gaps, default=None),
                attribution_policy='Every >=100 ms interval must contain >=100 ms of positively instrumented non-teardown main work or run-loop wait, excluding all teardown overlap, AND its main-thread teardown union must remain <100 ms, AND its unattributed remainder must remain <100 ms. Idle time overlapping off-main detach helpers is also excluded because indirect scheduling contention is unproved. Any failed conjunct rejects the causal exception.',
                gaps=sorted(gaps, key=lambda gap: (gap['start_ms'], gap['end_ms'], gap['source'])))


def analyze(root, native_events=None, fixture_pid=None):
    path = root / 'main-profile.jsonl'
    if not path.is_file():
        path = root / 'main-profile.jsonl.gz'
    rows = read_jsonl(path)
    native_events = native_events if native_events is not None else read_jsonl(root / 'teardown-events.jsonl')
    if fixture_pid is None:
        pids = {row.get('fixture_pid') for row in native_events}
        fixture_pid = next(iter(pids)) if len(pids) == 1 else None
    result = analyze_trace(rows, native_events, fixture_pid)
    result['trace_path'] = str(path)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence_directory', type=pathlib.Path)
    args = parser.parse_args()
    try:
        result = analyze(args.evidence_directory.resolve())
    except (OSError, ValueError, KeyError, TypeError) as error:
        result = dict(trace_valid=False, attribution_gate_passed=False, errors=[str(error)])
    print(json.dumps(result, indent=2))
    return 0 if result['attribution_gate_passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
