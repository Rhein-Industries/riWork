#!/usr/bin/env python3
"""Phase 6c: private, headless fixtures only; never starts a desktop or driver."""
import argparse
import json
from pathlib import Path
from phase1_integration import Verification, NSH, TSC
import phase1_integration

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['check', 'fixtures', 'suites', 'base', 'ui', 'ui-missing', 'focus', 'recovery'])
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--target-dir', required=True, type=Path)
    parser.add_argument('--ghostty-artifacts', required=True, type=Path)
    args = parser.parse_args()
    args.nsh, args.tsc, args.jobs = NSH, TSC, 2
    phase1_integration.ENV_KEYS += ('RIWORK_GHOSTTY_INSTALL_DIR', 'RIWORK_PENDING_OPEN_TEST_BUNDLE', 'RIWORK_SETTINGS_FOCUS_BUNDLE')
    run = Verification(args)
    # Account paths and injected credentials must not escape the private HOME.
    for key in ('CODEX_HOME', 'RIWORK_CODEX_ACCOUNT_HOME', 'ORCA_USER_DATA_PATH', 'ANTHROPIC_API_KEY', 'OPENAI_API_KEY'):
        run.env.pop(key, None)
    run.env['RIWORK_GHOSTTY_INSTALL_DIR'] = str(args.ghostty_artifacts.resolve())
    run.manifest.update(assignment='8fb9a8cb-761d-4c58-a3d6-44c53c24dc91', verification_environment='private HOME/RIWORK_HOME; headless fixtures and fake backends only', live_targets_touched='none beyond own shell/worktree', service_tier='default (Fast off); GPT-6-Astra ultra; system-default account', descendants_spawned=0)
    if args.mode == 'check':
        run.command(run.cargo('check', '-p', 'riwork-nsh-native', '-p', 'riwork-nsh-workspace'), 'cargo-check', timeout=600)
        run.command([NSH, 'check', 'apps/riwork', '--tsc', TSC], 'nsh-check', timeout=600)
    elif args.mode == 'fixtures':
        run.command(run.cargo('test', '-p', 'riwork-nsh-native', 'phase6c') + ['--', '--nocapture'], 'native-fixtures', timeout=600)
        run.command(run.cargo('test', '-p', 'riwork-desktop', '--bin', 'riwork', 'phase6c') + ['--', '--nocapture'], 'guard-fixture', timeout=600)
        for name in ['a_real_proxy_guards_unless_its_environment_opts_out', 'the_proxy_refuses_input_to_live_riwork_windows_and_forwards_the_rest', 'launch_rejects_missing_folder_or_empty_command_without_executing', 'input_limits_reject_nonfinite_positions_and_escaped_text_growth']:
            run.command(run.cargo('test', '-p', 'riwork-engine', name), name, timeout=600)
    elif args.mode == 'base':
        run.command(run.cargo('test', '-p', 'nsh-base'), 'base-suite', timeout=600)
    elif args.mode == 'suites':
        run.command(run.cargo('test', '-p', 'riwork-desktop', '--bin', 'riwork'), 'host-suite', timeout=600)
        run.command(run.cargo('test', '-p', 'riwork-nsh-workspace'), 'workspace-suite', timeout=600)
        run.command(run.cargo('test', '-p', 'riwork-nsh-native'), 'native-suite', timeout=600)
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
        if args.mode == 'ui-missing':
            (home / 'cua-fixture.json').write_text(json.dumps(dict(installed=False, ready=False, running=False, version=None, accessibility=False, screen_recording=False, direct_capture_verified=False, message='Cua Driver is not installed. Select Set up Cua or run `riwork setup`.', skills=dict(setup_needed=False, message='Fixture only; no driver invoked.', targets=[]))))
        run.env.update(RIWORK_HOME=str(home), RIWORK_NSH_FAKE_SESSIONS=str(home / 'fake-sessions.json'))
        if args.mode == 'focus':
            bundle = run.logdir / 'bundle/bundle.js'
            run.command([NSH, 'build', 'apps/riwork/SettingsCuaFocus.fixture.tsx', '--out', bundle.parent], 'focus-fixture-build', timeout=600)
            run.env['RIWORK_SETTINGS_FOCUS_BUNDLE'] = str(bundle.resolve())
            run.command(run.cargo('test', '-p', 'riwork-nsh-native', '--test', 'settings_cua_focus') + ['--', '--ignored', '--nocapture', '--test-threads=1'], 'focus-commands', timeout=600)
        run.command([NSH, 'test', 'apps/riwork', '--filter', {'ui-missing': 'CuaMissing', 'focus': 'P6C Focus'}.get(args.mode, 'P6C'), '--junit', run.logdir / 'phase6c.xml'], 'ui-fixtures', timeout=600)
    run.manifest['status'] = 'passed'
    run.save()
    print(run.manifest_path, flush=True)

if __name__ == '__main__':
    main()
