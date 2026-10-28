#!/usr/bin/env python3
"""Windowless App/native recovery regression; never launch a desktop process."""
import argparse
from pathlib import Path
from phase1_integration import Verification, NSH, TSC, REPO, digest
import phase1_integration


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['regression', 'ordering', 'suites'])
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--target-dir', required=True, type=Path)
    parser.add_argument('--ghostty-artifacts', required=True, type=Path)
    parser.add_argument('--expect-regression-failure', action='store_true')
    parser.add_argument('--bundle', type=Path, help='Reuse an already built App fixture, e.g. the pre-fix bundle')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    args.nsh, args.tsc = NSH, TSC
    phase1_integration.ENV_KEYS += ('RIWORK_GHOSTTY_INSTALL_DIR', 'RIWORK_PENDING_OPEN_TEST_BUNDLE')
    run = Verification(args)
    run.env['RIWORK_GHOSTTY_INSTALL_DIR'] = str(args.ghostty_artifacts.resolve())
    run.manifest.update(assignment='a6de9001-a3bd-484a-9cfe-50dedcaba8a5', verification_environment='isolated fixtures only', live_targets_touched='none beyond own shell/worktree', service_tier='default (Fast off)', descendants_spawned=0)
    run.manifest['source_sha256'] = {str(p): digest(REPO / p) for p in ['apps/riwork/App.tsx', 'apps/riwork/src/host_layouts.rs', 'crates/riwork-desktop/src/nsh_host.rs', 'crates/riwork-desktop/src/nsh_pending_open_tests.rs']}
    if args.mode in ('regression', 'ordering'):
        bundle = args.bundle or run.logdir / 'bundle/bundle.js'
        if not args.bundle:
            run.command([NSH, 'build', 'apps/riwork/PendingOpen.fixture.tsx', '--out', bundle.parent], 'app-fixture-build')
        run.env['RIWORK_PENDING_OPEN_TEST_BUNDLE'] = str(bundle.resolve())
        run.manifest['bundle_sha256'] = digest(bundle)
        code, out, err = run.command(run.cargo('test', '-p', 'riwork-desktop', '--bin', 'riwork', 'pending_open_tests::production_app_recovers_pending_open_after_failed_quit_and_restart') + ['--', '--ignored', '--nocapture', '--test-threads=1'], 'app-recovery', check=False, timeout=600)
        if args.expect_regression_failure:
            expected = b'lost ready acknowledgement' if args.mode == 'ordering' else b'waiting for app.restoreReady'
            assert code != 0 and expected in out + err, (code, out[-2000:], err[-2000:])
            run.manifest['status'] = 'expected regression reproduced'
        else:
            assert code == 0, 'App recovery failed; see raw command output'
            run.manifest['status'] = 'passed'
    else:
        run.command(run.cargo('test', '-p', 'riwork-desktop', '--bin', 'riwork'), 'host-suite', timeout=600)
        run.command(run.cargo('test', '-p', 'riwork-nsh-workspace'), 'layout-suites', timeout=600)
        run.command([NSH, 'check', 'apps/riwork', '--tsc', TSC], 'nsh-check', timeout=600)
        run.command(['cargo', 'fmt', '-p', 'riwork-desktop', '-p', 'riwork-nsh-workspace', '--check'], 'format')
        run.command(['git', 'diff', '--check'], 'whitespace')
        run.manifest['status'] = 'passed'
    run.save()
    print(run.manifest_path, flush=True)


if __name__ == '__main__':
    main()
