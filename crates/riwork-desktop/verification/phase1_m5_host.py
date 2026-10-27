#!/usr/bin/env python3
"""Run M5's host-owned named routes with disposable state and explicit gaps.

Uses the integration runner's isolated HOME/runtime/tmux environment and exact
command manifest. A successful Cargo filter matching zero tests is a failed
acceptance proof, never a pass. No desktop input or live-state read occurs.
"""
import argparse
import re
from pathlib import Path

from phase1_integration import NSH, TSC, Verification, write_json


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    args.mode, args.tsc, args.nsh = "host-named", TSC, NSH
    run = Verification(args)
    results = []

    def test(item, route, package, name, target=None, expected=True):
        argv = ["cargo", "test", "-p", package]
        if target:
            argv += ["--test", target]
        elif package == "riwork-desktop":
            argv += ["--bin", "riwork"]
        else:
            argv += ["--lib"]
        if not target:
            argv += [name]
        argv += ["--locked", "--offline"]
        if target == "appkit_shell":
            argv += ["--", name, "--test-threads=1"]
        code, stdout, _ = run.command(argv, f"{item}-{name.split('::')[-1]}", check=False, timeout=300)
        counts = re.findall(rb"test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored", stdout)
        passed = sum(int(row[0]) for row in counts)
        failed = sum(int(row[1]) for row in counts)
        status = "PASS" if code == 0 and passed and not failed else "FAIL"
        results.append(dict(item=item, route=route, test=name, argv=argv, exit_code=code,
                            passed=passed, failed=failed, status=status,
                            missing_proof=(passed == 0 and failed == 0),
                            successor=expected))

    for item, package, name in [
        ("P1-PROC-01", "riwork-cli", "only_gui_startup_forms_open_the_workspace"),
        ("P1-PROC-06", "riwork-engine", "live_gui_pids_lists_running_guis_and_changes_nothing"),
        ("P1-RLD-02", "riwork-engine", "queuing_and_wait_timeout_leave_old_gui_alive_and_do_not_launch_anything"),
        ("P1-RLD-02", "riwork-engine", "readiness_requires_correct_child_and_matching_registered_windows"),
        ("P1-RLD-02", "riwork-engine", "failed_or_hung_replacement_keeps_old_alive_and_allows_retry"),
        ("P1-RLD-02", "riwork-engine", "a_watchdog_that_fired_refuses_to_let_the_restore_confirm"),
        ("P1-RLD-03", "riwork-engine", "publishing_writes_only_changes_and_never_waits_for_the_registry_lock"),
        ("P1-RLD-04", "riwork-engine", "identifiers_geometry_and_private_file_read_are_validated"),
        ("P1-WIN-02", "riwork-core", "opening_windows_cascade_inside_the_display_and_clamp_after_cascading"),
        ("P1-WIN-04", "riwork-core", "legacy_and_invalid_window_sizes_do_not_break_layout_restoration"),
        ("P1-GLB-02", "riwork-core", "global_window::tests::"),
        ("P1-GLB-04", "riwork-engine", "the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows"),
    ]:
        test(item, "core", package, name)

    test("P1-PROC-02", "host-unit", "riwork-desktop", "gui_start_scrubs_child_ids", "startup_environment")
    for item, names in [
        ("P1-PROC-04", ["nsh_host::tests::folder_arg_reuses_existing_project", "nsh_host::tests::no_active_project_registers_the_cwd_fallback", "nsh_host::tests::a_vanished_project_root_is_an_error_for_the_caller_not_a_panic"]),
        ("P1-PROC-05", ["nsh_runtime::tests::forwards_path_and_reopen_without_creating_a_second_primary", "nsh_runtime::tests::reclaims_stale_socket_but_never_deletes_a_regular_file", "nsh_runtime::tests::replacement_keeps_old_socket_and_prevents_handoff_race"]),
        ("P1-RLD-02", ["nsh_runtime::tests::restore_acknowledgements_are_per_window_and_idempotent", "nsh_runtime::tests::restore_matches_targets_even_if_window_order_changes", "restore::tests::confirmation_gives_up_after_20_s"]),
        ("P1-RLD-03", ["nsh_host::tests::a_busy_registry_lock_stays_due"]),
        ("P1-RLD-04", ["nsh_host::tests::runtime_publication_reads_current_layout_without_an_appkit_state_event", "nsh_host::tests::runtime_frames_keep_the_existing_display_local_top_left_coordinates"]),
        ("P1-WIN-02", ["placement::tests::the_cascade_counts_open_non_tooltip_windows"]),
        ("P1-WIN-03", ["placement::tests::zoomed_counts_as_maximized_and_keeps_its_normal_frame", "placement::tests::fullscreen_counts_as_fullscreen"]),
        ("P1-WIN-07", ["window_registry::tests::the_last_close_quits", "window_registry::tests::tooltips_do_not_count"]),
        ("P1-WIN-08", ["nsh_host::tests::quit_flushes_every_layout_session_and_the_global_state_from_rust", "nsh_host::tests::close_flushes_the_last_accepted_edit_before_the_window_goes"]),
        ("P1-WIN-09", ["window_state::tests::events_only_on_change"]),
        ("P1-GLB-01", ["window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window"]),
        ("P1-GLB-04", ["runtime_window::tests::the_global_window_is_recorded_as_global"]),
        ("P1-GLB-08", ["menu::tests::dock_new_window"]),
        ("P1-GLB-09", ["window_registry::tests::route_prefers_frontmost_then_minimised_then_opens"]),
        ("P1-DRAG-03", ["host_names::tests::tab_transfer_is_refused"]),
        ("P1-MENU-01", ["nsh_menus::tests::command_ids_are_the_v1_set"]),
        ("P1-MENU-03", ["menu::tests::window_commands_go_to_the_key_window", "menu::tests::app_commands_work_with_no_window", "nsh_menus::tests::deferred_menu_commands_remain_present_disabled_and_keep_their_chords"]),
        ("P1-MENU-04", ["nsh_menus::tests::dock_labels_are_stable_unique_and_keep_the_last_key_window_marked"]),
        ("P1-KEYS-05", ["menu::tests::routing"]),
        ("P1-THM-05", ["startup::tests::forced_appearance_from_env"]),
    ]:
        for name in names:
            test(item, "host-unit", "riwork-desktop", name, expected=name.startswith("nsh_"))

    for item, name in [
        ("P1-WIN-01", "window_chrome"),
        ("P1-WIN-06", "titlebar_double_click_reads_the_system_preference"),
        ("P1-WIN-07", "tooltips_create_no_nswindow"),
        ("P1-MENU-01", "app_menu_matches_gpui"),
        ("P1-MENU-02", "edit_menu_uses_standard_selectors"),
        ("P1-MENU-03", "view_menu_matches_gpui"),
        ("P1-MENU-04", "dock_menu_from_a_fake_registry"),
        ("P1-KEYS-03", "app_level_key_equivalents"),
        ("P1-KEYS-05", "text_size_key_equivalents_dispatch_without_a_window"),
        ("P1-TXT-02", "view_menu_text_size_items"),
    ]:
        test(item, "appkit", "riwork-desktop", name, "appkit_shell")

    review_commands = [
        ["sed", "-n", "1,70p", "crates/riwork-desktop/src/nsh_main.rs"],
        ["rg", "-n", "set_ui_sets|AppView::new|RESTART_PENDING|app.restart|runtime_tick|system.fonts|Appearance::System", "crates/riwork-desktop/src/nsh_host.rs"],
        ["rg", "-n", "tab.transfer|tab.receive", "crates/riwork-desktop/src/nsh_host.rs"],
        ["git", "diff", "17e89d9", "--", "crates/riwork-desktop/src/main.rs", "crates/riwork-desktop/src/dock_menu.rs"],
    ]
    for index, argv in enumerate(review_commands):
        run.command(argv, f"review-{index}", check=False)
    run.manifest["named_routes"] = results
    run.manifest["status"] = "complete_with_acceptance_gaps"
    run.save()
    write_json(run.root / "host-named-results.json", dict(manifest=str(run.manifest_path), results=results))
    print(f"named-route results: {run.root / 'host-named-results.json'}")


if __name__ == "__main__":
    main()
