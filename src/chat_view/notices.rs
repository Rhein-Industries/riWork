//! The notices above the message box: what the provider or the driver said that is not part
//! of the conversation (a usage limit, a retry, a failed turn), and what went wrong in this
//! tab itself (the host could not be reached, a delete failed). docs/chat-notices.md is the
//! contract the phone shares.
//!
//! One banner per key: a notice's `kind`, else its text (a log written before `kind`
//! existed repeats the same notice under new ids), or the kind a newer notice with that
//! text has; a tab error's `LocalKey`. A newer
//! notice of a kind replaces the older one's banner, a resolved one takes it away, and a
//! dismissed sticky occurrence stays away across clients and restarts. Local closes
//! hide an item until a new id or a later reset arrives.

use std::collections::{HashMap, HashSet};

use crate::chat::model::{ItemBody, NoticeLevel, Transcript, notice_kind};

#[cfg(test)]
thread_local! {
    /// The time `now_unix` says in a test that sets one.
    pub(super) static TEST_NOW: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// How many banners show before the rest fold into "n more".
pub(super) const VISIBLE: usize = 2;

/// A tab error's key: the same trouble again updates its banner rather than adding one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum LocalKey {
    /// The host could not be reached, or the tab is not connected yet.
    Link,
    Delete,
    /// A message or its attachments did not go out.
    Send,
    Answer,
    Attachment,
    Settings,
    Media,
}

impl LocalKey {
    fn name(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Delete => "delete",
            Self::Send => "send",
            Self::Answer => "answer",
            Self::Attachment => "attachment",
            Self::Settings => "settings",
            Self::Media => "media",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Banner {
    /// The item id of a provider notice, `local:<key>` for a tab error: what × dismisses.
    pub id: String,
    pub level: NoticeLevel,
    pub text: String,
    pub kind: Option<String>,
    pub resets_at: Option<u64>,
    pub resolved: bool,
}

impl Banner {
    pub fn usage_limit(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with(&format!("{}:", notice_kind::RATE_LIMIT)))
    }
}

/// A kind that stays up across turns until it is resolved or dismissed.
fn sticky(kind: Option<&str>) -> bool {
    crate::chat::model::sticky_notice(kind)
}

#[derive(Default)]
pub(super) struct Notices {
    /// The tab's own errors, newest last, each with how many transcript items there were
    /// when it came: it is newer than those, older than the ones after.
    local: Vec<(LocalKey, NoticeLevel, String, usize)>,
    /// How many transcript items the tab has applied (`observe`).
    items: usize,
    /// Provider notices the user closed, by item id.
    dismissed: HashSet<String>,
    /// Reset captured by an optimistic local dismissal: a later reset can show again.
    dismissed_resets: HashMap<String, u64>,
    /// "n more" was pressed: every banner shows.
    pub expanded: bool,
    /// The history of every notice is open above the message box.
    pub history: bool,
    /// Banners whose chevron was pressed, by id: open (whole text) or closed (one line).
    pub opened: HashMap<String, bool>,
}

impl Notices {
    /// The transcript has `items` items now: a tab error from here on is newer than them.
    pub fn observe(&mut self, items: usize) {
        self.items = items;
    }

    /// Show `text` under `key`, replacing what was there.
    pub fn set(&mut self, key: LocalKey, level: NoticeLevel, text: impl Into<String>) {
        self.local.retain(|(at, ..)| *at != key);
        self.local.push((key, level, text.into(), self.items));
    }

    /// Its cause is gone. Whether there was one.
    pub fn clear(&mut self, key: LocalKey) -> bool {
        let before = self.local.len();
        self.local.retain(|(at, ..)| *at != key);
        self.local.len() != before
    }

    pub fn local(&self, key: LocalKey) -> Option<&str> {
        self.local
            .iter()
            .find(|(at, ..)| *at == key)
            .map(|(_, _, text, _)| text.as_str())
    }

    pub fn dismiss(&mut self, id: &str) {
        if let Some(name) = id.strip_prefix("local:") {
            self.local.retain(|(key, ..)| key.name() != name);
        } else {
            self.dismissed.insert(id.to_owned());
        }
    }

    pub fn dismiss_occurrence(&mut self, id: &str, transcript: &Transcript) {
        self.dismiss(id);
        if let Some(reset) = transcript.items.iter().find_map(|item| {
            if item.id != id {
                return None;
            }
            match &item.body {
                ItemBody::Notice { resets_at, .. } => *resets_at,
                _ => None,
            }
        }) {
            self.dismissed_resets.insert(id.to_owned(), reset);
        }
    }

    fn dismissed_for(&self, transcript: &Transcript) -> HashSet<String> {
        self.dismissed
            .iter()
            .filter(|id| {
                self.dismissed_resets.get(*id).is_none_or(|reset| {
                    let later_reset = transcript.items.iter().any(|item| {
                        item.id == **id
                            && matches!(item.body, ItemBody::Notice {
                                resets_at: Some(later), ..
                            } if later > *reset)
                    });
                    !later_reset
                })
            })
            .cloned()
            .collect()
    }

    /// The soonest time a shown banner's limit resets, when it must go without anything
    /// else happening.
    pub fn next_expiry(&self, transcript: &Transcript, now: u64) -> Option<u64> {
        provider_banners(transcript, &self.dismissed_for(transcript), now)
            .iter()
            .filter_map(|banner| banner.resets_at)
            .min()
    }

    /// What shows, newest first, the tab's errors among the provider's notices by when
    /// they came.
    pub fn banners(&self, transcript: &Transcript, now: u64) -> Vec<Banner> {
        // A notice at position p ranks 2p + 1; a tab error that came after n items, 2n:
        // newer than the items before it, older than the ones after.
        let mut ranked: Vec<((usize, usize), Banner)> = self
            .local
            .iter()
            .enumerate()
            .map(|(order, (key, level, text, after))| {
                (
                    (2 * after, order + 1),
                    Banner {
                        id: format!("local:{}", key.name()),
                        level: *level,
                        text: text.clone(),
                        kind: None,
                        resets_at: None,
                        resolved: false,
                    },
                )
            })
            .collect();
        ranked.extend(
            ranked_provider_banners(transcript, &self.dismissed_for(transcript), now)
                .into_iter()
                .map(|(at, banner)| ((2 * at + 1, 0), banner)),
        );
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        ranked.into_iter().map(|(_, banner)| banner).collect()
    }
}

fn banner(id: &str, body: &ItemBody) -> Option<Banner> {
    let ItemBody::Notice {
        level,
        text,
        kind,
        resolved,
        resets_at,
        ..
    } = body
    else {
        return None;
    };
    Some(Banner {
        id: id.to_owned(),
        level: *level,
        text: text.clone(),
        kind: kind.clone(),
        resets_at: *resets_at,
        resolved: *resolved,
    })
}

/// The provider's notices that show, newest first (see the module's rules).
pub(super) fn provider_banners(
    transcript: &Transcript,
    dismissed: &HashSet<String>,
    now: u64,
) -> Vec<Banner> {
    ranked_provider_banners(transcript, dismissed, now)
        .into_iter()
        .map(|(_, banner)| banner)
        .collect()
}

/// `provider_banners` with each one's position in the transcript.
fn ranked_provider_banners(
    transcript: &Transcript,
    dismissed: &HashSet<String>,
    now: u64,
) -> Vec<(usize, Banner)> {
    let exchange = transcript
        .items
        .iter()
        .rposition(|item| matches!(item.body, ItemBody::UserMessage { .. }))
        .unwrap_or(0);
    // A kind-less notice whose text a kinded one also has is that kind's older entry.
    let kinds: HashMap<&str, &str> = transcript
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Notice {
                kind: Some(kind),
                text,
                ..
            } => Some((text.as_str(), kind.as_str())),
            _ => None,
        })
        .collect();
    // The newest notice of each key.
    let mut newest: HashMap<String, usize> = HashMap::new();
    for (at, item) in transcript.items.iter().enumerate() {
        if let ItemBody::Notice { kind, text, .. } = &item.body {
            let key = match kind
                .as_deref()
                .or_else(|| kinds.get(text.as_str()).copied())
            {
                Some(kind) => format!("kind:{kind}"),
                None => format!("text:{text}"),
            };
            // A kinded notice keeps its key's banner (its usage action, its stickiness)
            // over a kind-less copy of its text.
            let kinded = |at: usize| {
                matches!(
                    &transcript.items[at].body,
                    ItemBody::Notice { kind: Some(_), .. }
                )
            };
            if kind.is_some() || newest.get(&key).is_none_or(|&was| !kinded(was)) {
                newest.insert(key, at);
            }
        }
    }
    let mut shown: Vec<usize> = newest
        .into_values()
        .filter(|&at| {
            let item = &transcript.items[at];
            let Some(notice) = banner(&item.id, &item.body) else {
                return false;
            };
            !notice.resolved
                && !matches!(
                    item.body,
                    ItemBody::Notice {
                        dismissed: true,
                        ..
                    }
                )
                && !dismissed.contains(&item.id)
                && notice.resets_at.is_none_or(|reset| reset > now)
                && (at >= exchange || sticky(notice.kind.as_deref()))
        })
        .collect();
    shown.sort_unstable_by(|a, b| b.cmp(a));
    shown
        .into_iter()
        .filter_map(|at| {
            banner(&transcript.items[at].id, &transcript.items[at].body).map(|b| (at, b))
        })
        .collect()
}

/// Every notice of the chat, newest first, resolved and dismissed ones too.
pub(super) fn history(transcript: &Transcript) -> Vec<Banner> {
    transcript
        .items
        .iter()
        .rev()
        .filter_map(|item| banner(&item.id, &item.body))
        .collect()
}

pub(super) fn count(transcript: &Transcript) -> usize {
    transcript
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::Notice { .. }))
        .count()
}

/// "resets today at 14:00", "resets Tue at 14:00", "resets 12 Oct at 14:00", local time.
pub(super) fn reset_text(resets_at: u64, now: u64) -> Option<String> {
    use chrono::{Local, TimeZone};
    let reset = Local
        .timestamp_opt(i64::try_from(resets_at).ok()?, 0)
        .single()?;
    let today = Local
        .timestamp_opt(i64::try_from(now).ok()?, 0)
        .single()?
        .date_naive();
    let days = (reset.date_naive() - today).num_days();
    let time = reset.format("%H:%M");
    Some(match days {
        0 => format!("resets today at {time}"),
        1 => format!("resets tomorrow at {time}"),
        2..=6 => format!("resets {} at {time}", reset.format("%a")),
        _ => format!("resets {} at {time}", reset.format("%-d %b")),
    })
}

pub(super) fn now_unix() -> u64 {
    #[cfg(test)]
    if let Some(now) = TEST_NOW.with(std::cell::Cell::get) {
        return now;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChatEvent, Item, ItemStatus};

    fn notice(id: &str, level: NoticeLevel, text: &str, kind: Option<&str>) -> Item {
        Item {
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            body: ItemBody::notice(level, text, kind),
            presentation: Default::default(),
        }
    }

    fn user(id: &str) -> Item {
        Item {
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            body: ItemBody::UserMessage { text: "hi".into() },
            presentation: Default::default(),
        }
    }

    fn transcript(items: Vec<Item>) -> Transcript {
        let mut t = Transcript::default();
        for item in items {
            t.apply(&ChatEvent::ItemCompleted { item });
        }
        t
    }

    fn texts(banners: &[Banner]) -> Vec<&str> {
        banners.iter().map(|b| b.text.as_str()).collect()
    }

    #[test]
    fn a_newer_notice_of_a_kind_replaces_the_older_banner_and_newest_comes_first() {
        let t = transcript(vec![
            user("u"),
            notice(
                "a",
                NoticeLevel::Warning,
                "close to",
                Some("rate_limit:seven_day"),
            ),
            notice("b", NoticeLevel::Error, "one-off", None),
            notice(
                "c",
                NoticeLevel::Error,
                "reached",
                Some("rate_limit:seven_day"),
            ),
        ]);
        let shown = provider_banners(&t, &HashSet::new(), 0);
        assert_eq!(texts(&shown), ["reached", "one-off"]);
    }

    #[test]
    fn repeated_notices_show_once_per_kind_and_older_kindless_ones_once_per_text() {
        let close = "This account is close to the weekly usage limit.";
        let t = transcript(vec![
            user("u"),
            // A log written before `kind`: the same notice under new ids.
            notice("old1", NoticeLevel::Warning, close, None),
            notice("old2", NoticeLevel::Warning, close, None),
            notice("x1", NoticeLevel::Error, "Overloaded", None),
            notice(
                "l1",
                NoticeLevel::Warning,
                close,
                Some("rate_limit:seven_day"),
            ),
            notice("r1", NoticeLevel::Warning, "retry 1", Some("api_retry")),
            notice("x2", NoticeLevel::Error, "Overloaded", None),
            notice(
                "l2",
                NoticeLevel::Error,
                "reached",
                Some("rate_limit:seven_day"),
            ),
            notice("r2", NoticeLevel::Warning, "retry 2", Some("api_retry")),
            notice("old3", NoticeLevel::Warning, close, None),
        ]);
        let shown = provider_banners(&t, &HashSet::new(), 0);
        let ids: Vec<&str> = shown.iter().map(|b| b.id.as_str()).collect();
        // `close` is also the text of a `rate_limit:seven_day` notice: one banner, the
        // kinded one, which keeps the usage action.
        assert_eq!(ids, ["r2", "l2", "x2"]);
    }

    #[test]
    fn resolved_dismissed_and_expired_notices_do_not_show() {
        let mut t = transcript(vec![
            user("u"),
            notice("retry", NoticeLevel::Warning, "retrying", Some("api_retry")),
            notice("x", NoticeLevel::Error, "closed", None),
            notice(
                "limit",
                NoticeLevel::Warning,
                "close",
                Some("rate_limit:five_hour"),
            ),
        ]);
        let mut resolved = t.items[1].clone();
        if let ItemBody::Notice { resolved: r, .. } = &mut resolved.body {
            *r = true;
        }
        t.apply(&ChatEvent::ItemCompleted { item: resolved });
        if let ItemBody::Notice { resets_at, .. } = &mut t.items[3].body {
            *resets_at = Some(100);
        }
        let dismissed = HashSet::from(["x".to_owned()]);
        assert_eq!(texts(&provider_banners(&t, &dismissed, 50)), ["close"]);
        assert!(provider_banners(&t, &dismissed, 100).is_empty());
        // The history keeps all of them, newest first.
        assert_eq!(texts(&history(&t)), ["close", "closed", "retrying"]);
    }

    #[test]
    fn only_usage_limits_and_sign_in_outlive_the_exchange_they_came_in() {
        let t = transcript(vec![
            user("u1"),
            notice("f", NoticeLevel::Error, "turn failed", Some("turn_failed")),
            notice(
                "l",
                NoticeLevel::Warning,
                "limit",
                Some("rate_limit:seven_day"),
            ),
            notice("a", NoticeLevel::Error, "sign in", Some("auth_required")),
            user("u2"),
            notice("w", NoticeLevel::Warning, "now", None),
        ]);
        assert_eq!(
            texts(&provider_banners(&t, &HashSet::new(), 0)),
            ["now", "sign in", "limit"]
        );
    }

    #[test]
    fn a_dismissed_notice_updated_in_place_stays_away_but_a_new_one_shows() {
        let mut notices = Notices::default();
        let mut t = transcript(vec![
            user("u"),
            notice(
                "r1",
                NoticeLevel::Warning,
                "Reconnecting 1/5",
                Some("reconnecting"),
            ),
        ]);
        notices.dismiss("r1");
        t.apply(&ChatEvent::ItemCompleted {
            item: notice(
                "r1",
                NoticeLevel::Warning,
                "Reconnecting 2/5",
                Some("reconnecting"),
            ),
        });
        assert!(notices.banners(&t, 0).is_empty());
        t.apply(&ChatEvent::ItemCompleted {
            item: notice(
                "r2",
                NoticeLevel::Warning,
                "Reconnecting 1/5",
                Some("reconnecting"),
            ),
        });
        assert_eq!(texts(&notices.banners(&t, 0)), ["Reconnecting 1/5"]);
    }

    #[test]
    fn a_tab_error_replaces_its_own_banner_and_clears_or_dismisses() {
        let mut notices = Notices::default();
        let t = Transcript::default();
        notices.set(
            LocalKey::Link,
            NoticeLevel::Error,
            "Could not reach the chat: a",
        );
        notices.set(LocalKey::Delete, NoticeLevel::Error, "Could not delete");
        notices.set(
            LocalKey::Link,
            NoticeLevel::Error,
            "Could not reach the chat: b",
        );
        assert_eq!(
            texts(&notices.banners(&t, 0)),
            ["Could not reach the chat: b", "Could not delete"]
        );
        assert!(notices.clear(LocalKey::Link));
        assert!(!notices.clear(LocalKey::Link));
        notices.dismiss("local:delete");
        assert!(notices.banners(&t, 0).is_empty());
    }

    #[test]
    fn tab_errors_and_provider_notices_stack_by_when_they_came() {
        let mut notices = Notices::default();
        let mut t = transcript(vec![
            user("u"),
            notice("old", NoticeLevel::Warning, "old notice", None),
        ]);
        notices.observe(t.items.len());
        notices.set(LocalKey::Link, NoticeLevel::Error, "link");
        notices.set(LocalKey::Delete, NoticeLevel::Error, "delete");
        t.apply(&ChatEvent::ItemCompleted {
            item: notice("new", NoticeLevel::Error, "sign in", Some("auth_required")),
        });
        assert_eq!(
            texts(&notices.banners(&t, 0)),
            ["sign in", "delete", "link", "old notice"]
        );
        // A kind that only starts like a usage limit is not one.
        let banner = |kind: &str| Banner {
            id: "x".into(),
            level: NoticeLevel::Info,
            text: String::new(),
            kind: Some(kind.into()),
            resets_at: None,
            resolved: false,
        };
        assert!(banner("rate_limit:codex").usage_limit());
        assert!(!banner("rate_limit_configuration").usage_limit());
    }

    #[test]
    fn reset_times_read_as_a_day_and_a_time() {
        use chrono::{Local, TimeZone};
        let now = Local.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap();
        let at = |days: i64, hour: u32| {
            (now.date_naive() + chrono::Days::new(days as u64))
                .and_hms_opt(hour, 30, 0)
                .unwrap()
                .and_local_timezone(Local)
                .unwrap()
                .timestamp() as u64
        };
        let now = now.timestamp() as u64;
        assert_eq!(reset_text(at(0, 14), now).unwrap(), "resets today at 14:30");
        assert_eq!(
            reset_text(at(1, 8), now).unwrap(),
            "resets tomorrow at 08:30"
        );
        assert_eq!(reset_text(at(3, 8), now).unwrap(), "resets Sun at 08:30");
        assert_eq!(
            reset_text(at(20, 8), now).unwrap(),
            "resets 28 Oct at 08:30"
        );
    }
}

#[cfg(test)]
mod dismissal_tests {
    use super::*;
    use crate::chat::model::{ChatEvent, Item, ItemStatus};

    #[test]
    fn notice_with_a_later_reset_overcomes_optimistic_local_dismissal_of_the_same_id() {
        let make = |reset| {
            let mut body = ItemBody::notice(
                NoticeLevel::Warning,
                "weekly limit",
                Some("rate_limit:seven_day"),
            );
            if let ItemBody::Notice { resets_at, .. } = &mut body {
                *resets_at = Some(reset);
            }
            Item {
                id: "limit".into(),
                turn_id: None,
                status: ItemStatus::Completed,
                body,
                presentation: Default::default(),
            }
        };
        let mut transcript = Transcript::default();
        transcript.apply(&ChatEvent::ItemCompleted { item: make(100) });
        let mut notices = Notices::default();
        notices.dismiss_occurrence("limit", &transcript);
        assert!(notices.banners(&transcript, 0).is_empty());
        transcript.apply(&ChatEvent::ItemCompleted { item: make(100) });
        assert!(notices.banners(&transcript, 0).is_empty());
        transcript.apply(&ChatEvent::ItemCompleted { item: make(200) });
        assert_eq!(notices.banners(&transcript, 0).len(), 1);
    }
}
