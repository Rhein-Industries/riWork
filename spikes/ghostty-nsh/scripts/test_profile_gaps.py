#!/usr/bin/env python3
"""Regression tests for the causal gate; no apps, subprocesses, or GUI input."""
import copy
import unittest

from profile_gaps import analyze_trace


def fixture(extra=(), delayed_free=False):
    # Each tuple is (time, name, phase, surface, main_thread).
    late = 130 if delayed_free else 0
    values = [
        (0, 'profile-start', 'i', 0, True),
        (1, 'create-surface', 'b', 1, True),
        (5, 'create-surface', 'e', 1, True),
        (10, 'heartbeat', 'i', 0, True),
        (132, 'heartbeat', 'i', 0, True),
        (150, 'shutdown-dispatch', 'b', 1, True),
        (151, 'detach-confirm-wait', 'b', 1, False),
        (152, 'shutdown-dispatch', 'e', 1, True),
        (153, 'detach-confirm-wait', 'e', 1, False),
        (154, 'closing-tick', 'b', 1, True),
        (154.1, 'child-exit-check', 'b', 1, True),
        (154.2, 'child-exit-check', 'e', 1, True),
        (155, 'surface-free', 'b', 1, True),
        (156 + late, 'surface-free', 'e', 1, True),
        (157 + late, 'app-config-free', 'b', 1, True),
        (158 + late, 'app-config-free', 'e', 1, True),
        (159 + late, 'closing-tick', 'e', 1, True),
        (170 + late, 'heartbeat', 'i', 0, True),
        (171 + late, 'process-exit', 'i', 0, True),
    ] + list(extra)
    events = [dict(seq=index, monotonic_ms=timestamp, name=name, phase=phase,
                   surface_id=sid, thread_id=1 if main else 2, main_thread=main)
              for index, (timestamp, name, phase, sid, main) in enumerate(sorted(values))]
    rows = [dict(kind='metadata', clock='CLOCK_MONOTONIC', fixture_pid=123,
                 recorded=len(events), capacity=10000, dropped=0)] + events
    native = [dict(event='created', surface_id=1, fixture_pid=123, monotonic_ms=5),
              dict(event='main-loop-gap', surface_id=1, fixture_pid=123, monotonic_ms=154,
                   gap_start_ms=132, gap_end_ms=154, gap_ms=22)]
    return rows, native


class AttributionTests(unittest.TestCase):
    def analyze(self, extra=(), delayed_free=False):
        rows, native = fixture(extra, delayed_free)
        return analyze_trace(rows, native, 123)

    def test_positive_attached_work_explains_gap(self):
        result = self.analyze([(11, 'attached-tick', 'b', 1, True),
                               (131, 'attached-tick', 'e', 1, True)])
        self.assertTrue(result['trace_valid'], result['errors'])
        self.assertFalse(result['raw_all_gaps_under_100ms'])
        self.assertTrue(result['attribution_gate_passed'])
        self.assertEqual(result['gaps'][0]['positive_non_teardown_ms'], 120)

    def test_positive_runloop_sleep_explains_timer_delivery(self):
        result = self.analyze([(11, 'runloop-before-waiting', 'i', 0, True),
                               (131, 'runloop-after-waiting', 'i', 0, True)])
        self.assertTrue(result['attribution_gate_passed'], result['errors'])
        self.assertEqual(result['gaps'][0]['overlap_union_ms_by_name']['runloop-idle-wait'], 120)

    def test_unknown_delay_is_never_inferred_unrelated(self):
        result = self.analyze()
        self.assertTrue(result['trace_valid'])
        self.assertFalse(result['attribution_gate_passed'])
        self.assertEqual(result['gaps'][0]['classification'], 'unknown')

    def test_short_unrelated_work_does_not_exonerate_unknown_remainder(self):
        result = self.analyze([(11, 'attached-tick', 'b', 1, True),
                               (91, 'attached-tick', 'e', 1, True)])
        self.assertFalse(result['attribution_gate_passed'])

    def test_free_stall_still_fails_with_other_unrelated_gap(self):
        result = self.analyze([(11, 'attached-tick', 'b', 1, True),
                               (131, 'attached-tick', 'e', 1, True)], delayed_free=True)
        self.assertFalse(result['attribution_gate_passed'])
        self.assertIn('surface-free', [gap['classification'] for gap in result['gaps']])

    def test_one_gap_with_long_unrelated_work_and_long_teardown_fails(self):
        rows, native = fixture([(11, 'attached-tick', 'b', 1, True),
                                (131, 'attached-tick', 'e', 1, True)], delayed_free=True)
        events = [event for event in rows[1:]
                  if not (event['name'] == 'heartbeat' and event['monotonic_ms'] == 132)]
        for index, event in enumerate(events):
            event['seq'] = index
        rows[0]['recorded'] = len(events)
        result = analyze_trace([rows[0]] + events, native, 123)
        self.assertTrue(result['trace_valid'], result['errors'])
        self.assertFalse(result['attribution_gate_passed'])
        gap = result['gaps'][0]
        self.assertEqual(gap['gap_ms'], 290)
        self.assertEqual(gap['positive_non_teardown_ms'], 120)
        self.assertEqual(gap['teardown_main_union_ms'], 137)
        self.assertIn('main-thread teardown union at least 100 ms', gap['rejection_reasons'])

    def test_long_unrelated_work_cannot_exonerate_long_unknown_remainder(self):
        rows, native = fixture([(11, 'attached-tick', 'b', 1, True),
                                (131, 'attached-tick', 'e', 1, True)])
        events = [event for event in rows[1:]
                  if not (event['name'] == 'heartbeat' and event['monotonic_ms'] == 132)]
        for index, event in enumerate(events):
            event['seq'] = index
            if event['monotonic_ms'] >= 170:
                event['monotonic_ms'] += 300
        rows[0]['recorded'] = len(events)
        result = analyze_trace([rows[0]] + events, native, 123)
        self.assertTrue(result['trace_valid'], result['errors'])
        self.assertFalse(result['attribution_gate_passed'])
        gap = result['gaps'][0]
        self.assertEqual(gap['positive_non_teardown_ms'], 120)
        self.assertLess(gap['teardown_main_union_ms'], 100)
        self.assertGreaterEqual(gap['unattributed_ms'], 100)
        self.assertIn('unattributed remainder at least 100 ms', gap['rejection_reasons'])

    def test_detach_worker_wait_is_context_not_main_thread_evidence(self):
        rows, native = fixture()
        for event in rows[1:]:
            if event.get('name') == 'detach-confirm-wait' and event['phase'] == 'b':
                event['monotonic_ms'] = 11
        ordered = sorted(rows[1:], key=lambda event: event['monotonic_ms'])
        for index, event in enumerate(ordered):
            event['seq'] = index
        result = analyze_trace([rows[0]] + ordered, native, 123)
        self.assertTrue(result['trace_valid'], result['errors'])
        self.assertFalse(result['attribution_gate_passed'])
        self.assertGreater(result['gaps'][0]['overlap_union_ms_by_name']['detach-confirm-wait'], 100)

    def test_nested_scopes_are_counted_once(self):
        result = self.analyze([(11, 'attached-tick', 'b', 1, True),
                               (12, 'attached-app-tick', 'b', 1, True),
                               (90, 'attached-app-tick', 'e', 1, True),
                               (91, 'attached-tick', 'e', 1, True)])
        self.assertEqual(result['gaps'][0]['positive_non_teardown_ms'], 80)
        self.assertFalse(result['attribution_gate_passed'])

    def test_incomplete_or_dropped_spans_fail(self):
        rows, native = fixture()
        broken = copy.deepcopy(rows)
        broken[0]['dropped'] = 1
        self.assertFalse(analyze_trace(broken, native, 123)['trace_valid'])
        broken = copy.deepcopy(rows)
        broken[-2]['phase'] = 'b'
        self.assertFalse(analyze_trace(broken, native, 123)['trace_valid'])

    def test_intervening_main_work_invalidates_idle_proof(self):
        result = self.analyze([(11, 'runloop-before-waiting', 'i', 0, True),
                               (50, 'runloop-entry', 'i', 0, True),
                               (51, 'runloop-exit', 'i', 0, True),
                               (131, 'runloop-after-waiting', 'i', 0, True)])
        self.assertFalse(result['trace_valid'])
        self.assertFalse(result['attribution_gate_passed'])

    def test_surface_gap_endpoints_must_match_reported_interval(self):
        rows, native = fixture()
        native[-1]['gap_ms'] = 120
        self.assertFalse(analyze_trace(rows, native, 123)['trace_valid'])

    def test_one_leading_partial_runloop_is_explicitly_supported(self):
        result = self.analyze([(6, 'runloop-exit', 'i', 0, True),
                               (7, 'runloop-entry', 'i', 0, True),
                               (8, 'runloop-exit', 'i', 0, True)])
        self.assertTrue(result['trace_valid'], result['errors'])
        observed = result['runloop_observation']
        self.assertFalse(observed['leading_partial_loop']['entry_observed'])
        self.assertEqual(observed['leading_partial_loop']['exit_ms'], 6)
        self.assertEqual(observed['observed_entries'], 1)
        self.assertEqual(observed['observed_exits'], 2)
        self.assertEqual(observed['final_nesting_depth'], 0)
        # The omitted entry provides no positive attribution for the long gap.
        self.assertFalse(result['attribution_gate_passed'])

    def test_second_unpaired_runloop_exit_is_rejected(self):
        result = self.analyze([(6, 'runloop-exit', 'i', 0, True),
                               (7, 'runloop-exit', 'i', 0, True)])
        self.assertFalse(result['trace_valid'])
        self.assertTrue(any('unpaired run-loop exit' in error for error in result['errors']))

    def test_unpaired_exit_after_complete_loop_is_not_leading(self):
        result = self.analyze([(6, 'runloop-entry', 'i', 0, True),
                               (7, 'runloop-exit', 'i', 0, True),
                               (8, 'runloop-exit', 'i', 0, True)])
        self.assertFalse(result['trace_valid'])
        self.assertIsNone(result['runloop_observation']['leading_partial_loop'])

    def test_idle_during_detach_helper_cannot_prove_unrelated_delay(self):
        rows, native = fixture([(12, 'runloop-before-waiting', 'i', 0, True),
                                (131, 'runloop-after-waiting', 'i', 0, True)])
        for event in rows[1:]:
            if event['name'] == 'detach-confirm-wait' and event['phase'] == 'b':
                event['monotonic_ms'] = 11
        ordered = sorted(rows[1:], key=lambda event: event['monotonic_ms'])
        for index, event in enumerate(ordered):
            event['seq'] = index
        result = analyze_trace([rows[0]] + ordered, native, 123)
        self.assertTrue(result['trace_valid'], result['errors'])
        self.assertFalse(result['attribution_gate_passed'])
        self.assertEqual(result['gaps'][0]['positive_non_teardown_ms'], 0)
        self.assertEqual(result['gaps'][0]['idle_ms_excluded_for_detach_overlap'], 119)


if __name__ == '__main__':
    unittest.main()
