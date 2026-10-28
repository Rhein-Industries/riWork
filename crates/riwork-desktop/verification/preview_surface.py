#!/usr/bin/env python3
"""TERM-183..189: private headless/fake verification, never a desktop launch."""
import argparse
from pathlib import Path
from phase1_integration import Verification, NSH, TSC
import phase1_integration

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['check', 'surface', 'suites', 'ui', 'keys', 'focus', 'recovery'])
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--target-dir', required=True, type=Path)
    parser.add_argument('--ghostty-artifacts', required=True, type=Path)
    args = parser.parse_args()
    args.nsh, args.tsc, args.jobs = NSH, TSC, 2
    phase1_integration.ENV_KEYS += ('RIWORK_GHOSTTY_INSTALL_DIR', 'RIWORK_PENDING_OPEN_TEST_BUNDLE', 'RIWORK_PREVIEW_FOCUS_BUNDLE')
    run = Verification(args)
    for key in ('CODEX_HOME', 'RIWORK_CODEX_ACCOUNT_HOME', 'ORCA_USER_DATA_PATH', 'ANTHROPIC_API_KEY', 'OPENAI_API_KEY'):
        run.env.pop(key, None)
    run.env['RIWORK_GHOSTTY_INSTALL_DIR'] = str(args.ghostty_artifacts.resolve())
    run.manifest.update(assignment='9319ad32-317b-4d3a-aba7-203a09f43b23', verification_environment='private HOME/RIWORK_HOME; headless fixtures/fakes; no GUI/Cua, network, Docker or real preview processes', live_targets_touched='none beyond own shell/worktree', service_tier='default, Fast OFF (user verified); GPT-6-Astra ultra/system-default account unchanged', descendants_spawned=2)
    if args.mode == 'check':
        run.command(run.cargo('check', '-p', 'riwork-nsh-native', '-p', 'riwork-desktop', '--bin', 'riwork'), 'cargo-check', timeout=600)
        run.command([NSH, 'check', 'apps/riwork', '--tsc', TSC], 'nsh-check', timeout=600)
    elif args.mode == 'surface':
        run.command(run.cargo('test', '-p', 'riwork-nsh-native', 'preview') + ['--', '--nocapture'], 'surface-fixtures', timeout=600)
        run.command(run.cargo('test', '-p', 'riwork-engine', 'app_preview::tests::logs_have_a_byte_and_entry_bound_even_without_newlines'), 'unchanged-engine-log-bounds', timeout=600)
    elif args.mode == 'suites':
        for package, name, extra in [('riwork-desktop', 'host-suite', ['--bin', 'riwork']), ('riwork-nsh-workspace', 'workspace-suite', []), ('riwork-nsh-native', 'native-suite', [])]:
            run.command(run.cargo('test', '-p', package, *extra), name, timeout=600)
        run.command(['cargo', 'fmt', '-p', 'riwork-desktop', '-p', 'riwork-nsh-workspace', '-p', 'riwork-nsh-native', '--check'], 'format')
        run.command(['git', 'diff', '--check'], 'whitespace')
    elif args.mode == 'recovery':
        bundle = run.logdir / 'bundle/bundle.js'
        run.command([NSH, 'build', 'apps/riwork/PendingOpen.fixture.tsx', '--out', bundle.parent], 'app-fixture-build', timeout=600)
        run.env['RIWORK_PENDING_OPEN_TEST_BUNDLE'] = str(bundle.resolve())
        run.command(run.cargo('test', '-p', 'riwork-desktop', '--bin', 'riwork', 'pending_open_tests::production_app_recovers_pending_open_after_failed_quit_and_restart') + ['--', '--ignored', '--nocapture', '--test-threads=1'], 'app-recovery', timeout=600)
    else:
        home = run.logdir / 'nsh-home'
        run.command(run.cargo('run', '-p', 'riwork-nsh-native', '--features', 'test-support', '--example', 'p1_workspace_fixture_home') + ['--', '--scenario', 'p1-layouts', '--out', home], 'seed-fixture', timeout=600)
        run.env.update(RIWORK_HOME=str(home), RIWORK_NSH_FAKE_SESSIONS=str(home / 'fake-sessions.json'))
        if args.mode == 'focus':
            bundle = run.logdir / 'bundle/bundle.js'
            run.command([NSH, 'build', 'apps/riwork/PreviewFocus.fixture.tsx', '--out', bundle.parent], 'native-focus-build', timeout=600)
            run.env['RIWORK_PREVIEW_FOCUS_BUNDLE'] = str(bundle.resolve())
            run.command(run.cargo('test', '-p', 'riwork-nsh-native', '--test', 'preview_focus') + ['--', '--ignored', '--nocapture', '--test-threads=1'], 'native-focus-wiring', timeout=600)
        elif args.mode == 'keys':
            run.command([NSH, 'test', 'apps/riwork', '--filter', 'P1-KEYS', '--junit', run.logdir / 'keys.xml'], 'existing-keyboard-ui', timeout=600)
        else:
            run.command([NSH, 'test', 'apps/riwork', '--filter', 'P6C', '--junit', run.logdir / 'preview.xml'], 'ui-fixtures', timeout=600)
    run.manifest['status'] = 'passed'
    run.save()
    print(run.manifest_path, flush=True)

if __name__ == '__main__':
    main()
