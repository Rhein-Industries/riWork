//! Which hidden terminal tabs give up their Ghostty surface, and when.
//!
//! A terminal tab is a tmux client drawn by its own Ghostty surface: a renderer,
//! IO threads, several window-sized render targets and a `login` child. The
//! session, its process and its scrollback live in tmux, so a tab that is not on
//! screen can drop the surface (which only detaches that client) and attach a new
//! one when it is shown again. This module is the policy only. It knows nothing
//! about GPUI: the workspace describes its panes, applies the answer and re-asks
//! at `Plan::recheck`.

use std::time::Duration;

use crate::layouts::PaneId;

pub type TabId = u64;

/// Hidden terminals per window that keep their surface however briefly they were
/// hidden, most recently visible first, so flipping between a few tabs stays
/// instant.
pub const WARM_HIDDEN: usize = 3;
/// How long a terminal outside the warm set must stay hidden before it is released.
pub const RELEASE_GRACE: Duration = Duration::from_secs(30);
/// Even a warm terminal is released after this long, so an idle window does not
/// hold on to surfaces for hours.
pub const WARM_EXPIRY: Duration = Duration::from_secs(10 * 60);
/// Surfaces released per pass. Tearing one down waits for its client to exit on
/// the UI thread, so a burst is spread over a few passes instead of stalling it.
pub const MAX_RELEASES_PER_PASS: usize = 2;
/// How often a pass repeats while a drag, menu or modal freezes releases.
pub const FROZEN_RECHECK: Duration = Duration::from_secs(5);
/// How often a pass repeats while only pinned terminals are waiting.
pub const PINNED_RECHECK: Duration = Duration::from_secs(15);
/// The shortest wait between passes.
pub const MIN_RECHECK: Duration = Duration::from_millis(250);

/// One tab of a pane, as far as the policy cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabState {
    pub id: TabId,
    /// The tab holds a surface right now. Panels and already released tabs do not.
    pub attached: bool,
    /// Keep the surface whatever else is true. Used where a new attach could not
    /// bring the screen back, such as a session that has ended.
    pub pinned: bool,
    /// How long the tab has been off screen. Ignored while it is shown.
    pub hidden_for: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub id: PaneId,
    pub active: usize,
    pub tabs: Vec<TabState>,
}

/// The whole window, as the policy sees it.
#[derive(Clone, Debug)]
pub struct Screen<'a> {
    pub panes: &'a [PaneState],
    pub active_pane: PaneId,
    /// Focus mode shows the active pane alone.
    pub focus_mode: bool,
    /// A tab drag, pane menu or modal holds the shown terminals frozen as
    /// snapshots.
    pub frozen: bool,
}

/// Whether tab `index` of a pane is on screen. Every pane shows its active tab,
/// locked or not; focus mode shows only the active pane's. A tab drag, menu or
/// modal covers the terminals with snapshots but does not change which tab is
/// current, so it is not part of this.
pub fn is_shown(
    index: usize,
    pane_active: usize,
    pane: PaneId,
    active_pane: PaneId,
    focus_mode: bool,
) -> bool {
    index == pane_active && (!focus_mode || pane == active_pane)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Tabs whose surface should be dropped now, longest hidden first.
    pub release: Vec<TabId>,
    /// When to ask again. `None` when no hidden surface is waiting.
    pub recheck: Option<Duration>,
}

/// Decide which hidden terminals to release.
///
/// * A shown terminal is never released, and neither is a pinned one.
/// * Nothing is released while the screen is frozen: the shown terminals are
///   being snapshotted or dragged around and may change hands.
/// * The `WARM_HIDDEN` most recently visible hidden terminals stay until
///   `WARM_EXPIRY`; every other hidden terminal goes after `RELEASE_GRACE`.
/// * At most `MAX_RELEASES_PER_PASS` per pass; the rest follow shortly.
pub fn plan(screen: &Screen<'_>) -> Plan {
    let mut hidden = Vec::new();
    let mut pinned_waiting = false;
    for pane in screen.panes {
        for (index, tab) in pane.tabs.iter().enumerate() {
            if !tab.attached
                || is_shown(
                    index,
                    pane.active,
                    pane.id,
                    screen.active_pane,
                    screen.focus_mode,
                )
            {
                continue;
            }
            if tab.pinned {
                pinned_waiting = true;
            } else {
                hidden.push(tab);
            }
        }
    }
    if screen.frozen {
        let waiting = pinned_waiting || !hidden.is_empty();
        return Plan {
            release: Vec::new(),
            recheck: waiting.then_some(FROZEN_RECHECK),
        };
    }
    // Most recently visible first. Tabs hidden at the same moment (focus mode
    // hides several panes at once) keep the newer tab warm.
    hidden.sort_by(|a, b| a.hidden_for.cmp(&b.hidden_for).then(b.id.cmp(&a.id)));
    let mut due = Vec::new();
    let mut wait = pinned_waiting.then_some(PINNED_RECHECK);
    for (rank, tab) in hidden.iter().enumerate() {
        let after = if rank < WARM_HIDDEN {
            WARM_EXPIRY
        } else {
            RELEASE_GRACE
        };
        if tab.hidden_for >= after {
            due.push(tab.id);
        } else {
            let remaining = after - tab.hidden_for;
            wait = Some(wait.map_or(remaining, |wait| wait.min(remaining)));
        }
    }
    // `due` is newest first; the longest hidden are released first.
    due.reverse();
    if due.len() > MAX_RELEASES_PER_PASS {
        due.truncate(MAX_RELEASES_PER_PASS);
        wait = Some(MIN_RECHECK);
    }
    Plan {
        release: due,
        recheck: wait.map(|wait| wait.max(MIN_RECHECK)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn tab(id: TabId, hidden_for: Duration) -> TabState {
        TabState {
            id,
            attached: true,
            pinned: false,
            hidden_for,
        }
    }

    fn pinned(id: TabId, hidden_for: Duration) -> TabState {
        TabState {
            pinned: true,
            ..tab(id, hidden_for)
        }
    }

    fn pane(id: PaneId, active: usize, tabs: Vec<TabState>) -> PaneState {
        PaneState { id, active, tabs }
    }

    /// One pane whose first tab is the shown one.
    fn single(tabs: Vec<TabState>) -> Vec<PaneState> {
        vec![pane(1, 0, tabs)]
    }

    fn plan_for(panes: &[PaneState], active_pane: PaneId, focus_mode: bool, frozen: bool) -> Plan {
        plan(&Screen {
            panes,
            active_pane,
            focus_mode,
            frozen,
        })
    }

    /// Plain layout, nothing frozen, pane 1 active.
    fn plan_of(tabs: Vec<TabState>) -> Plan {
        plan_for(&single(tabs), 1, false, false)
    }

    #[test]
    fn a_shown_terminal_is_never_released_however_old_its_clock_is() {
        let plan = plan_of(vec![tab(1, secs(3600)), tab(2, secs(3600))]);
        assert!(!plan.release.contains(&1));
        assert_eq!(plan.release, vec![2]);
    }

    #[test]
    fn every_panes_active_tab_is_shown_and_only_the_others_are_judged() {
        let panes = vec![
            pane(1, 1, vec![tab(1, secs(100)), tab(2, secs(3600))]),
            pane(
                2,
                0,
                vec![
                    tab(3, secs(3600)),
                    tab(4, secs(50)),
                    tab(5, secs(60)),
                    tab(6, secs(70)),
                    tab(7, secs(80)),
                ],
            ),
        ];
        let plan = plan_for(&panes, 1, false, false);
        // Tabs 2 and 3 are the panes' active tabs, so their stale clocks do not
        // matter. Of the rest, 4, 5 and 6 are the warmest and 7 and 1 are past
        // the grace period.
        assert_eq!(plan.release, vec![1, 7]);
    }

    #[test]
    fn the_warm_set_keeps_the_most_recently_visible_hidden_terminals() {
        let plan = plan_of(vec![
            tab(1, secs(0)),
            tab(2, secs(100)),
            tab(3, secs(200)),
            tab(4, secs(300)),
            tab(5, secs(400)),
            tab(6, secs(500)),
        ]);
        // Tab 1 is shown; 2, 3 and 4 are the three most recently visible.
        assert_eq!(plan.release, vec![6, 5]);
    }

    #[test]
    fn a_short_hide_keeps_the_terminal_even_outside_the_warm_set() {
        let plan = plan_of(vec![
            tab(1, secs(0)),
            tab(2, secs(1)),
            tab(3, secs(2)),
            tab(4, secs(3)),
            tab(5, secs(29)),
        ]);
        assert!(plan.release.is_empty());
        // The fifth tab is one second short of the grace period.
        assert_eq!(plan.recheck, Some(secs(1)));
    }

    #[test]
    fn the_grace_period_ends_exactly_at_thirty_seconds() {
        let mut tabs = vec![tab(1, secs(0)), tab(2, secs(2)), tab(3, secs(3))];
        tabs.push(tab(4, secs(4)));
        tabs.push(tab(5, RELEASE_GRACE - Duration::from_millis(1)));
        assert!(plan_of(tabs.clone()).release.is_empty());
        tabs[4].hidden_for = RELEASE_GRACE;
        assert_eq!(plan_of(tabs).release, vec![5]);
    }

    #[test]
    fn a_warm_terminal_is_released_after_the_warm_expiry() {
        let plan = plan_of(vec![tab(1, secs(0)), tab(2, WARM_EXPIRY - secs(1))]);
        assert!(plan.release.is_empty());
        assert_eq!(plan.recheck, Some(secs(1)));
        let plan = plan_of(vec![tab(1, secs(0)), tab(2, WARM_EXPIRY)]);
        assert_eq!(plan.release, vec![2]);
    }

    #[test]
    fn a_pinned_terminal_is_kept_and_does_not_take_a_warm_slot() {
        // Tab 2 is pinned and was hidden most recently of all.
        let mut tabs = vec![tab(1, secs(0)), pinned(2, secs(1))];
        tabs.extend((3..=7).map(|id| tab(id, secs(100 * id))));
        let plan = plan_of(tabs);
        assert!(!plan.release.contains(&2));
        // Tabs 3, 4 and 5 are warm; 6 and 7 are released, longest hidden first.
        assert_eq!(plan.release, vec![7, 6]);
    }

    #[test]
    fn a_pinned_terminal_alone_is_only_polled_slowly() {
        let plan = plan_of(vec![tab(1, secs(0)), pinned(2, secs(3600))]);
        assert!(plan.release.is_empty());
        assert_eq!(plan.recheck, Some(PINNED_RECHECK));
    }

    #[test]
    fn a_tab_without_a_surface_is_left_alone() {
        let released = TabState {
            attached: false,
            ..tab(2, secs(3600))
        };
        assert_eq!(plan_of(vec![tab(1, secs(0)), released]), Plan::default());
    }

    #[test]
    fn a_locked_pane_shows_its_active_tab_like_any_other_pane() {
        // Locking a pane keeps its tabs open; it does not change what is on screen,
        // so the policy takes no lock flag. Pane 1 is a locked pane holding several
        // terminals and pane 2 is the one being worked in: both keep their active
        // tab, and the locked pane's other tabs are judged like anyone else's.
        let mut locked = vec![tab(10, secs(0))];
        locked.extend((11..=15).map(|id| tab(id, secs(100 + id))));
        let panes = vec![
            pane(1, 0, locked),
            pane(2, 0, vec![tab(20, secs(0)), tab(21, secs(40))]),
        ];
        let plan = plan_for(&panes, 2, false, false);
        assert!(!plan.release.contains(&10), "the locked pane's shown tab");
        assert!(!plan.release.contains(&20), "the active pane's shown tab");
        // Warm: 21, 11 and 12. The rest are past the grace period.
        assert_eq!(plan.release, vec![15, 14]);
    }

    #[test]
    fn a_locked_pane_stays_shown_while_it_is_not_the_active_pane() {
        let panes = vec![
            pane(1, 0, vec![tab(1, secs(3600))]),
            pane(2, 0, vec![tab(2, secs(0))]),
        ];
        assert_eq!(plan_for(&panes, 2, false, false), Plan::default());
    }

    #[test]
    fn focus_mode_shows_only_the_active_panes_tab() {
        assert!(is_shown(0, 0, 1, 2, false));
        assert!(is_shown(0, 0, 2, 2, true));
        assert!(!is_shown(0, 0, 1, 2, true));
        assert!(
            !is_shown(1, 0, 2, 2, true),
            "a background tab is never shown"
        );
        let panes = vec![
            pane(1, 0, vec![tab(1, secs(0))]),
            pane(2, 0, vec![tab(2, secs(0))]),
            pane(3, 0, vec![tab(3, secs(0))]),
        ];
        // Outside focus mode all three are shown and nothing waits.
        assert_eq!(plan_for(&panes, 2, false, false), Plan::default());
        // In focus mode panes 1 and 3 are hidden, but only just: they stay warm
        // and are looked at again when they would expire.
        let plan = plan_for(&panes, 2, true, false);
        assert!(plan.release.is_empty());
        assert_eq!(plan.recheck, Some(WARM_EXPIRY));
    }

    #[test]
    fn focus_mode_lets_the_other_panes_go_once_they_have_been_hidden_long_enough() {
        let mut panes = vec![pane(9, 0, vec![tab(9, secs(0))])];
        panes.extend((1..=5).map(|id| pane(id, 0, vec![tab(id, secs(60 + id))])));
        // Five hidden panes: three stay warm, the two longest hidden go.
        assert_eq!(plan_for(&panes, 9, true, false).release, vec![5, 4]);
        // Without focus mode all six tabs are shown.
        assert!(plan_for(&panes, 9, false, false).release.is_empty());
    }

    #[test]
    fn a_frozen_screen_releases_nothing() {
        let mut tabs = vec![tab(1, secs(0))];
        tabs.extend((2..=9).map(|id| tab(id, secs(10_000))));
        let plan = plan_for(&single(tabs), 1, false, true);
        assert!(plan.release.is_empty());
        assert_eq!(plan.recheck, Some(FROZEN_RECHECK));
    }

    #[test]
    fn a_frozen_screen_with_nothing_hidden_needs_no_recheck() {
        let panes = single(vec![tab(1, secs(0))]);
        assert_eq!(plan_for(&panes, 1, false, true), Plan::default());
    }

    #[test]
    fn a_drag_does_not_hide_the_shown_tabs_and_ends_where_it_started() {
        // A drag freezes the screen but the shown tabs stay the shown tabs: once it
        // ends the same layout is judged again and they are still safe.
        let mut tabs = vec![tab(1, secs(0))];
        tabs.extend((2..=9).map(|id| tab(id, secs(10_000 + id))));
        let panes = single(tabs);
        let frozen = plan_for(&panes, 1, false, true);
        let thawed = plan_for(&panes, 1, false, false);
        assert!(frozen.release.is_empty());
        assert!(!thawed.release.contains(&1));
        assert_eq!(thawed.release, vec![9, 8]);
    }

    #[test]
    fn releases_are_spread_over_passes() {
        let mut tabs = vec![tab(1, secs(0))];
        tabs.extend((2..=12).map(|id| tab(id, secs(1000 + id))));
        let plan = plan_of(tabs);
        assert_eq!(plan.release.len(), MAX_RELEASES_PER_PASS);
        assert_eq!(plan.recheck, Some(MIN_RECHECK));
    }

    #[test]
    fn tabs_hidden_at_the_same_moment_keep_the_newer_tab_warm() {
        let mut tabs = vec![tab(1, secs(0))];
        tabs.extend((2..=6).map(|id| tab(id, secs(60))));
        // 6, 5 and 4 are warm because they are the newest tabs; 2 and 3 are not.
        assert_eq!(plan_of(tabs).release, vec![2, 3]);
    }

    #[test]
    fn the_recheck_targets_the_next_terminal_to_come_due() {
        let plan = plan_of(vec![
            tab(1, secs(0)),
            tab(2, secs(1)),
            tab(3, secs(2)),
            tab(4, secs(3)),
            tab(5, secs(10)),
            tab(6, secs(4)),
        ]);
        // Tabs 2, 3 and 4 are warm. Tabs 6 and 5 wait out the grace period, and 5,
        // hidden longer, comes due first.
        assert!(plan.release.is_empty());
        assert_eq!(plan.recheck, Some(RELEASE_GRACE - secs(10)));
    }

    #[test]
    fn nothing_hidden_means_nothing_to_recheck() {
        assert_eq!(plan_of(vec![tab(1, secs(0))]), Plan::default());
        assert_eq!(
            plan_for(&[], 1, false, false),
            Plan::default(),
            "an empty window"
        );
    }
}
