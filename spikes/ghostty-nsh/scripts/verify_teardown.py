#!/usr/bin/env python3
"""Verify Phase 2 teardown captures without operating an app or tmux server.

Usage: verify_teardown.py RUN_DIRECTORY [--expected-min-cycles 20]
Required captures: runtime.json, teardown-events.jsonl, cycles.jsonl, and one
detach-SURFACE_ID.json for every created surface. A cycle is a batch of four
simultaneously attached surfaces followed by their disposal. Missing evidence
never establishes success. Exit 0: GO; 1: observed failure; 2: incomplete capture.
Runtime hashes are recorded declarations, not independent binary provenance.
"""
import argparse
import collections
import json
import math
import pathlib
import re
import sys


DISPOSAL_BOUND_MS = 2000.0
FREE_BOUND_MS = 100.0
SURFACES_PER_CYCLE = 4
REQUIRED_EVENTS = (
    'created', 'shutdown-requested', 'shutdown-returned', 'detach-confirmed',
    'child-exit-confirmed', 'free-begin', 'free-end', 'disposed',
)


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def pid(value):
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def surface_id(value):
    if isinstance(value, bool) or not isinstance(value, (int, str)) or not str(value).strip():
        raise ValueError(f'invalid surface_id: {value!r}')
    return str(value)


def jsonl(path):
    rows = []
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        if line.strip():
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError(f'{path.name}:{line_number} must contain an object')
            rows.append(row)
    return rows


def verify(root, expected_min_cycles=20):
    checks = {}
    missing = []

    def check(name, passed, **observed):
        checks[name] = {'status': 'PASS' if passed else 'FAIL', **observed}

    def read(name, lines=False):
        path = root / name
        if not path.is_file():
            missing.append(name)
            return [] if lines else {}
        result = jsonl(path) if lines else json.loads(path.read_text())
        if not lines and not isinstance(result, dict):
            raise ValueError(f'{name} must contain an object')
        return result

    runtime = read('runtime.json')
    events = read('teardown-events.jsonl', lines=True)
    cycles = read('cycles.jsonl', lines=True)
    if runtime:
        text_fields = ('runtime_label', 'archive_path', 'header_path')
        hash_fields = ('archive_sha256', 'header_sha256', 'fixture_sha256')
        check('runtime_recorded',
              all(isinstance(runtime.get(key), str) and runtime[key].strip() for key in text_fields)
              and all(isinstance(runtime.get(key), str)
                      and re.fullmatch(r'[0-9a-fA-F]{64}', runtime[key]) for key in hash_fields),
              declaration=runtime,
              limitation='Hashes identify the declared artifacts; this verifier does not authenticate their provenance.')

    by_surface = collections.defaultdict(list)
    for row in events:
        by_surface[surface_id(row['surface_id'])].append(row)
    fixture_pids = sorted({row.get('fixture_pid') for row in events if pid(row.get('fixture_pid'))})
    if events:
        check('one_fixture_process', len(fixture_pids) == 1
              and all(row.get('fixture_pid') == fixture_pids[0] for row in events),
              fixture_pids=fixture_pids)
        check('no_teardown_failure', not any(row.get('event') == 'teardown-failed' for row in events),
              failures=[row for row in events if row.get('event') == 'teardown-failed'])
    else:
        missing.append('nonempty teardown-events.jsonl')
    fixture_pid = fixture_pids[0] if len(fixture_pids) == 1 else None

    helpers = {}
    lifetimes = {}
    identities = []
    free_durations = []
    disposal_durations = []
    request_durations = []
    main_free_durations = []
    loop_gaps = []
    final_callback_durations = []
    for sid, rows in sorted(by_surface.items()):
        counts = collections.Counter(row.get('event') for row in rows)
        unique = all(counts[event] == 1 for event in REQUIRED_EVENTS)
        check(f'surface_{sid}_complete', unique, event_counts=dict(counts))
        gap_rows = [row for row in rows if row.get('event') == 'main-loop-gap']
        gaps = [row.get('gap_ms') for row in gap_rows]
        check(f'surface_{sid}_responsive_main_loop', bool(gaps)
              and all(row.get('main_thread') is True for row in gap_rows)
              and all(number(gap) and 0 <= gap < FREE_BOUND_MS for gap in gaps),
              samples=len(gaps), max_gap_ms=max(gaps) if gaps and all(number(gap) for gap in gaps) else None)
        loop_gaps.extend(gap for gap in gaps if number(gap))
        helper = read(f'detach-{sid}.json')
        if helper:
            helpers[sid] = helper
            before, after = helper.get('before'), helper.get('after')
            clients_valid = (isinstance(before, list) and isinstance(after, list)
                             and all(isinstance(client, dict) and pid(client.get('pid'))
                                     and isinstance(client.get('tty'), str)
                                     and isinstance(client.get('session'), str)
                                     for client in before + after))
            target = [client for client in before if isinstance(client, dict)
                      and client.get('pid') == helper.get('client_pid')
                      and client.get('tty') == helper.get('tty')] if clients_valid else []
            identity_before, identity_after = helper.get('identity_before'), helper.get('identity_after')
            if isinstance(identity_before, str) and identity_before.strip():
                identities.extend([identity_before, identity_after])
            check(f'surface_{sid}_target_detached',
                  helper.get('surface_id') is not None and surface_id(helper['surface_id']) == sid
                  and fixture_pid is not None and helper.get('fixture_pid') == fixture_pid
                  and pid(helper.get('client_pid')) and pid(helper.get('wrapper_pid'))
                  and helper['client_pid'] != fixture_pid and helper['wrapper_pid'] != fixture_pid
                  and isinstance(helper.get('tty'), str) and bool(helper['tty'])
                  and clients_valid and len(target) == 1 and helper.get('detached') is True
                  and all(client['pid'] != helper['client_pid'] and client['tty'] != helper['tty'] for client in after)
                  and isinstance(identity_before, str) and bool(identity_before.strip())
                  and identity_before == identity_after,
                  evidence=helper)
        if not unique:
            continue
        selected = {event: next(row for row in rows if row.get('event') == event) for event in REQUIRED_EVENTS}
        times = {event: row.get('monotonic_ms') for event, row in selected.items()}
        valid_times = all(number(value) and value >= 0 for value in times.values())
        final_gap_time = gap_rows[-1].get('monotonic_ms') if gap_rows else None
        final_callback_ms = (times['disposed'] - final_gap_time
                             if number(final_gap_time) and number(times['disposed']) else None)
        check(f'surface_{sid}_final_callback_bounded', number(final_callback_ms)
              and 0 <= final_callback_ms < FREE_BOUND_MS,
              last_timer_sample_to_disposed_ms=final_callback_ms)
        if number(final_callback_ms):
            final_callback_durations.append(final_callback_ms)
        ordered = (valid_times
                   and times['created'] <= times['shutdown-requested']
                   and times['shutdown-requested'] <= times['shutdown-returned'] < times['free-begin']
                   and times['shutdown-requested'] <= times['detach-confirmed'] < times['free-begin']
                   and times['shutdown-requested'] <= times['child-exit-confirmed'] < times['free-begin']
                   and times['free-begin'] <= times['free-end'] <= times['disposed'])
        # File order supplements timestamps: equal-resolution clocks cannot turn
        # a pre-barrier free into a passing run.
        positions = {event: next(index for index, row in enumerate(rows) if row.get('event') == event)
                     for event in REQUIRED_EVENTS}
        ordered = (ordered and positions['created'] < positions['shutdown-requested']
                   < positions['detach-confirmed'] < positions['free-begin']
                   and positions['shutdown-requested'] < positions['shutdown-returned'] < positions['free-begin']
                   and positions['shutdown-requested'] < positions['child-exit-confirmed'] < positions['free-begin']
                   < positions['free-end'] < positions['disposed'])
        check(f'surface_{sid}_detach_and_child_exit_before_free', ordered, monotonic_ms=times)
        main_events = ('shutdown-requested', 'shutdown-returned', 'free-begin', 'free-end', 'disposed')
        check(f'surface_{sid}_main_thread', all(selected[event].get('main_thread') is True for event in main_events))
        free_ms = selected['free-end'].get('free_ms')
        disposal_ms = selected['disposed'].get('disposal_ms')
        request_ms = selected['shutdown-returned'].get('main_block_ms')
        main_free_ms = selected['disposed'].get('main_free_block_ms')
        # main_block_ms describes the initial asynchronous shutdown dispatch;
        # free_ms separately measures the synchronous native free on the UI.
        duration_values = number(free_ms) and number(disposal_ms) and number(request_ms) and number(main_free_ms)
        measured_free = times['free-end'] - times['free-begin'] if valid_times else None
        measured_disposal = times['disposed'] - times['shutdown-requested'] if valid_times else None
        bounded = (duration_values and valid_times
                   and 0 <= free_ms < FREE_BOUND_MS and 0 <= measured_free < FREE_BOUND_MS
                   and 0 <= request_ms < FREE_BOUND_MS
                   and 0 <= main_free_ms < FREE_BOUND_MS
                   and 0 <= disposal_ms < DISPOSAL_BOUND_MS and 0 <= measured_disposal < DISPOSAL_BOUND_MS
                   and abs(free_ms - measured_free) <= 1.0
                   # shutdownStart precedes tty lookup and the request log.
                   # The reported full duration must conservatively cover the
                   # shorter request-log-to-disposed interval, not equal it.
                   and disposal_ms >= measured_disposal - 1.0)
        check(f'surface_{sid}_bounded_disposal', bounded,
              free_ms=free_ms, disposal_ms=disposal_ms, shutdown_main_block_ms=request_ms,
              main_free_block_ms=main_free_ms,
              timestamp_free_ms=measured_free, timestamp_disposal_ms=measured_disposal)
        if duration_values:
            free_durations.append(free_ms)
            disposal_durations.append(disposal_ms)
            request_durations.append(request_ms)
            main_free_durations.append(main_free_ms)
        if valid_times:
            lifetimes[sid] = times

    paired = collections.defaultdict(dict)
    cycle_order = []
    for index, row in enumerate(cycles):
        key = str(row['cycle'])
        phase = row['phase']
        if phase not in ('attached', 'closed'):
            raise ValueError(f'cycles.jsonl has unknown phase: {phase!r}')
        if phase in paired[key]:
            raise ValueError(f'cycle {key} has duplicate {phase} rows')
        paired[key][phase] = (index, row)
        if key not in cycle_order:
            cycle_order.append(key)
    check('required_completed_cycles', len(paired) >= expected_min_cycles
          and all(set(pair) == {'attached', 'closed'} for pair in paired.values()),
          required=expected_min_cycles, observed=len(paired), surfaces_per_cycle=SURFACES_PER_CYCLE)
    assigned = []
    prior_closed_time = -1
    prior_native_disposed = -1
    prior_bytes = prior_sequence = 0
    batch_durations = []
    for key in cycle_order:
        pair = paired[key]
        if set(pair) != {'attached', 'closed'}:
            continue
        attached_index, attached = pair['attached']
        closed_index, closed = pair['closed']
        a_ids_raw, c_ids_raw = attached.get('surface_ids'), closed.get('surface_ids')
        ids_valid = isinstance(a_ids_raw, list) and isinstance(c_ids_raw, list)
        a_ids = [surface_id(value) for value in a_ids_raw] if ids_valid else []
        c_ids = [surface_id(value) for value in c_ids_raw] if ids_valid else []
        assigned.extend(a_ids)
        cycle_helpers = [helpers[sid] for sid in a_ids if sid in helpers]
        expected_clients = {helper.get('client_pid') for helper in cycle_helpers if pid(helper.get('client_pid'))}
        old_pids = expected_clients | {helper.get('wrapper_pid') for helper in cycle_helpers if pid(helper.get('wrapper_pid'))}
        a_clients, c_clients = attached.get('clients'), closed.get('clients')
        client_pids = [client.get('pid') for client in a_clients if isinstance(client, dict)] if isinstance(a_clients, list) else []
        check(f'cycle_{key}_four_attached_then_zero_clients', ids_valid
              and len(a_ids) == SURFACES_PER_CYCLE and len(set(a_ids)) == SURFACES_PER_CYCLE
              and set(a_ids) == set(c_ids) and len(c_ids) == SURFACES_PER_CYCLE
              and len(cycle_helpers) == SURFACES_PER_CYCLE
              and len(expected_clients) == SURFACES_PER_CYCLE
              and len(client_pids) == SURFACES_PER_CYCLE and set(client_pids) == expected_clients
              and c_clients == [],
              surface_ids=a_ids, attached_client_pids=client_pids,
              expected_client_pids=sorted(expected_clients), closed_clients=c_clients)
        check(f'cycle_{key}_capture_postconditions', attached.get('ready') is True
              and closed.get('ready') is True and attached.get('all_disposed') is False
              and closed.get('all_disposed') is True
              and attached.get('fixture_pid') == fixture_pid == closed.get('fixture_pid')
              and attached.get('fixture_alive') is True
              and isinstance(closed.get('fixture_alive'), bool),
              attached_ready=attached.get('ready'), closed_ready=closed.get('ready'),
              attached_all_disposed=attached.get('all_disposed'), closed_all_disposed=closed.get('all_disposed'),
              attached_fixture_alive=attached.get('fixture_alive'), closed_fixture_alive=closed.get('fixture_alive'))
        alive = closed.get('alive_pids')
        descendants = closed.get('descendants')
        check(f'cycle_{key}_no_leaked_processes', isinstance(alive, list)
              and all(pid(value) for value in alive) and not (set(alive) & old_pids)
              and alive == [] and descendants == [],
              checked_old_pids=sorted(old_pids), alive_pids=alive, descendants=descendants)
        a_time, c_time = attached.get('monotonic_ms'), closed.get('monotonic_ms')
        times_valid = number(a_time) and number(c_time)
        covered = len(a_ids) == SURFACES_PER_CYCLE and all(sid in lifetimes for sid in a_ids)
        batch_ms = (max(lifetimes[sid]['disposed'] for sid in a_ids)
                    - min(lifetimes[sid]['shutdown-requested'] for sid in a_ids)) if covered else None
        temporal = (times_valid and attached_index < closed_index
                    and prior_closed_time <= a_time <= c_time and covered
                    and all(prior_native_disposed < lifetimes[sid]['created'] for sid in a_ids)
                    and max(lifetimes[sid]['created'] for sid in a_ids)
                    <= min(lifetimes[sid]['shutdown-requested'] for sid in a_ids)
                    and 0 <= batch_ms < DISPOSAL_BOUND_MS)
        check(f'cycle_{key}_bounded_batch', temporal, batch_disposal_ms=batch_ms,
              attached_monotonic_ms=a_time, closed_monotonic_ms=c_time)
        if batch_ms is not None:
            batch_durations.append(batch_ms)
        if times_valid:
            prior_closed_time = c_time
        if covered:
            prior_native_disposed = max(lifetimes[sid]['disposed'] for sid in a_ids)
        a_bytes, c_bytes = attached.get('pressure_bytes'), closed.get('pressure_bytes')
        a_sequence, c_sequence = attached.get('pressure_sequence'), closed.get('pressure_sequence')
        pressure_valid = all(isinstance(value, int) and not isinstance(value, bool)
                             for value in (a_bytes, c_bytes, a_sequence, c_sequence))
        check(f'cycle_{key}_live_output_pressure', pressure_valid
              and 0 <= prior_bytes < a_bytes < c_bytes
              and 0 <= prior_sequence < a_sequence < c_sequence,
              attached_bytes=a_bytes, closed_bytes=c_bytes,
              attached_sequence=a_sequence, closed_sequence=c_sequence)
        if pressure_valid:
            prior_bytes, prior_sequence = c_bytes, c_sequence
        identities.extend([attached.get('identity'), closed.get('identity')])
    check('all_created_surfaces_exercised_once', len(assigned) == len(by_surface)
          and len(assigned) >= expected_min_cycles * SURFACES_PER_CYCLE
          and len(set(assigned)) == len(assigned) and set(assigned) == set(by_surface),
          created_surfaces=len(by_surface), assigned_surfaces=len(assigned), disposed_surfaces=len(lifetimes))
    check('same_session_and_pane_survived_every_close', bool(identities)
          and all(isinstance(identity, str) and identity.strip() and identity == identities[0] for identity in identities),
          identity=identities[0] if identities else None, observations=len(identities))

    failed = [name for name, result in checks.items() if result['status'] == 'FAIL']
    # An incomplete capture can contain actual failures (for example a free
    # entered without a detach barrier). Preserve both categories in the output.
    readiness = 'NO_GO' if failed else 'UNVERIFIED' if missing else 'GO'
    return {
        'status': 'VERIFIED' if readiness == 'GO' else 'FAILED' if failed else 'INCOMPLETE',
        'overall_readiness': readiness,
        'evidence_directory': str(root),
        'fixture_pid': fixture_pid,
        'runtime': runtime,
        'bounds': {'disposal_ms_exclusive': DISPOSAL_BOUND_MS,
                   'native_free_ms_exclusive': FREE_BOUND_MS,
                   'shutdown_dispatch_ms_exclusive': FREE_BOUND_MS,
                   'minimum_cycles': expected_min_cycles, 'surfaces_per_cycle': SURFACES_PER_CYCLE},
        'observed_maxima_ms': {
            'free': max(free_durations, default=None),
            'disposal': max(disposal_durations, default=None),
            'shutdown_dispatch': max(request_durations, default=None),
            'main_free_block': max(main_free_durations, default=None),
            'main_loop_gap': max(loop_gaps, default=None),
            'final_callback': max(final_callback_durations, default=None),
            'batch_disposal': max(batch_durations, default=None),
        },
        'failed_checks': failed,
        'missing_evidence': sorted(set(missing)),
        'checks': checks,
        'clock_domains': {
            'native': 'clock_gettime(CLOCK_MONOTONIC), used for native ordering and duration bounds.',
            'capture': 'Python time.monotonic (mach_absolute_time on this macOS), used for capture ordering only.',
            'correlation': 'Explicit surface_ids and capture ready/all_disposed readbacks join domains. No cross-clock offsets or timestamp inequalities are inferred.',
        },
        'scope': 'Read-only capture verification; no desktop, process-control, or tmux operations. No recovery is inferred.',
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('evidence_directory', type=pathlib.Path)
    parser.add_argument('--expected-min-cycles', type=int, default=20)
    args = parser.parse_args()
    if args.expected_min_cycles < 20:
        parser.error('--expected-min-cycles must be at least 20 for the Phase 2 gate')
    try:
        result = verify(args.evidence_directory.resolve(), args.expected_min_cycles)
    except (OSError, ValueError, TypeError, KeyError, IndexError) as error:
        result = {'status': 'EVIDENCE_ERROR', 'overall_readiness': 'UNVERIFIED', 'error': str(error)}
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return {'GO': 0, 'NO_GO': 1, 'UNVERIFIED': 2}[result['overall_readiness']]


if __name__ == '__main__':
    sys.exit(main())
