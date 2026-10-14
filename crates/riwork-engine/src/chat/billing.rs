//! What a Claude chat's usage-limit banner offers, decided from the notice, how the chat
//! is billed and whether an API key is saved, so every client decides it the same way.
//!
//! A subscription window that rejects the chat (`rate_limit:five_hour`,
//! `rate_limit:seven_day`, see `rate_limits`) is one an Anthropic API key gets past: the
//! banner offers **Use API key** when a key is saved, **Enter API key** when none is. The
//! windows exist only for a subscription, so the notice itself says the chat ran on one;
//! once its Claude says it bills an API key (`Account::api_key_source`), the banner goes.
//! `overage` is left out: Claude names extra usage, the pay-as-you-go allowance some plans
//! have beyond the subscription, with it, and its rejection says that allowance is spent or
//! off, not that the subscription's window is. The model windows (`seven_day_opus`,
//! `seven_day_sonnet`) keep the plain banner: another model gets past them too.

use super::model::{Account, Item, ItemBody, NoticeLevel, Provider};

/// The subscription window a rejected limit notice of `kind` names, as the banner says it.
pub fn subscription_limit(kind: &str) -> Option<&'static str> {
    match kind.strip_prefix("rate_limit:")? {
        "five_hour" => Some("five-hour"),
        "seven_day" => Some("weekly"),
        _ => None,
    }
}

/// The way past a subscription limit the banner offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitOffer {
    /// A key is saved: switch the chat to it.
    UseApiKey,
    /// No key is saved: open Settings at the key field.
    EnterApiKey,
}

/// What the banner of a notice shows about billing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitBanner {
    /// Not a subscription limit of a Claude chat: the banner as it always was.
    Plain,
    /// A subscription limit, with the way past it.
    Offer(LimitOffer),
    /// A subscription limit of a chat that bills an API key now, or one that is over
    /// (resolved, or dismissed for this limit and reset time): no banner.
    Hidden,
}

/// The banner of `notice` in a chat of `provider` whose Claude said `account` at its last
/// start, with `key_saved` saying whether the Keychain has a key.
pub fn limit_banner(
    provider: Provider,
    notice: &ItemBody,
    account: Option<&Account>,
    key_saved: bool,
) -> LimitBanner {
    let ItemBody::Notice {
        level,
        kind: Some(kind),
        resolved,
        dismissed,
        ..
    } = notice
    else {
        return LimitBanner::Plain;
    };
    if provider != Provider::Claude
        || *level != NoticeLevel::Error
        || subscription_limit(kind).is_none()
    {
        return LimitBanner::Plain;
    }
    if *resolved || *dismissed || account.is_some_and(|account| account.api_key_source.is_some()) {
        return LimitBanner::Hidden;
    }
    LimitBanner::Offer(if key_saved {
        LimitOffer::UseApiKey
    } else {
        LimitOffer::EnterApiKey
    })
}

/// The message to send again once the chat is on its API key: that of the turn the limit
/// notice `notice_id` rejected, when the turn was rejected before it produced any item
/// (only notices lie between the message and the rejection), the message carried no
/// image or attachment (its text alone would not be the same message), and nothing was
/// sent since. Otherwise none: the chat only switches.
pub fn rejected_prompt(items: &[Item], notice_id: &str) -> Option<String> {
    let at = items.iter().position(|item| item.id == notice_id)?;
    if items[at + 1..]
        .iter()
        .any(|item| matches!(item.body, ItemBody::UserMessage { .. }))
    {
        return None;
    }
    for item in items[..at].iter().rev() {
        match &item.body {
            ItemBody::Notice { .. } => continue,
            ItemBody::UserMessage { text } => {
                return (item.presentation.is_empty() && !text.trim().is_empty())
                    .then(|| text.clone());
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChatImage, ImageSource, ItemStatus};

    fn notice(kind: &str, level: NoticeLevel) -> ItemBody {
        ItemBody::Notice {
            level,
            text: "This account has reached the five-hour usage limit.".into(),
            kind: Some(kind.into()),
            resolved: false,
            dismissed: false,
            resets_at: Some(1_767_225_600),
        }
    }

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            presentation: Default::default(),
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            body,
        }
    }

    fn user(id: &str, text: &str) -> Item {
        item(id, ItemBody::UserMessage { text: text.into() })
    }

    fn agent(id: &str) -> Item {
        item(
            id,
            ItemBody::AgentMessage {
                text: "on it".into(),
            },
        )
    }

    fn limit(id: &str) -> Item {
        item(id, notice("rate_limit:five_hour", NoticeLevel::Error))
    }

    #[test]
    fn a_rejected_subscription_window_offers_the_key_unless_the_chat_bills_one_already() {
        use LimitBanner::{Hidden, Offer, Plain};
        use LimitOffer::{EnterApiKey, UseApiKey};
        let subscription = Account {
            api_key_source: None,
            plan: Some("Claude Max".into()),
        };
        let api_key = Account {
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
            plan: None,
        };
        for kind in ["rate_limit:five_hour", "rate_limit:seven_day"] {
            let rejected = notice(kind, NoticeLevel::Error);
            for account in [None, Some(&subscription)] {
                assert_eq!(
                    limit_banner(Provider::Claude, &rejected, account, true),
                    Offer(UseApiKey),
                    "{kind}"
                );
                assert_eq!(
                    limit_banner(Provider::Claude, &rejected, account, false),
                    Offer(EnterApiKey),
                    "{kind}"
                );
            }
            // Already on an API key, whatever is saved: no banner.
            for saved in [false, true] {
                assert_eq!(
                    limit_banner(Provider::Claude, &rejected, Some(&api_key), saved),
                    Hidden
                );
            }
            // Over: resolved, or dismissed for this limit and reset time.
            for over in [
                |body: &mut ItemBody| {
                    if let ItemBody::Notice { resolved, .. } = body {
                        *resolved = true;
                    }
                },
                |body: &mut ItemBody| {
                    if let ItemBody::Notice { dismissed, .. } = body {
                        *dismissed = true;
                    }
                },
            ] {
                let mut done = rejected.clone();
                over(&mut done);
                assert_eq!(limit_banner(Provider::Claude, &done, None, true), Hidden);
            }
            // A warning is no rejection; another provider's limit is not Claude's.
            assert_eq!(
                limit_banner(
                    Provider::Claude,
                    &notice(kind, NoticeLevel::Warning),
                    None,
                    true
                ),
                Plain
            );
            for provider in [Provider::Codex, Provider::Muse] {
                assert_eq!(limit_banner(provider, &rejected, None, true), Plain);
            }
        }
        // Extra usage and the model windows keep the banner they had.
        for kind in [
            "rate_limit:overage",
            "rate_limit:seven_day_opus",
            "rate_limit:seven_day_sonnet",
            "rate_limit:unknown",
            "auth_required",
        ] {
            assert_eq!(
                limit_banner(
                    Provider::Claude,
                    &notice(kind, NoticeLevel::Error),
                    None,
                    true
                ),
                Plain,
                "{kind}"
            );
        }
        assert_eq!(
            limit_banner(
                Provider::Claude,
                &ItemBody::AgentMessage { text: "x".into() },
                None,
                true
            ),
            Plain
        );
        assert_eq!(
            subscription_limit("rate_limit:five_hour"),
            Some("five-hour")
        );
        assert_eq!(subscription_limit("rate_limit:seven_day"), Some("weekly"));
    }

    #[test]
    fn only_a_turn_rejected_before_it_produced_anything_is_sent_again() {
        // Rejected at once: its message, then notices, then the limit.
        let quiet = [
            user("u1", "first"),
            agent("a1"),
            user("u2", "fix the build"),
            item("retry", notice("api_retry", NoticeLevel::Warning)),
            limit("limit"),
        ];
        assert_eq!(
            rejected_prompt(&quiet, "limit").as_deref(),
            Some("fix the build")
        );
        // What Claude says after the rejection (its own error message) does not count.
        let mut answered_after = quiet.to_vec();
        answered_after.push(agent("synthetic"));
        assert_eq!(
            rejected_prompt(&answered_after, "limit").as_deref(),
            Some("fix the build")
        );
        // The turn had produced an item before it was rejected: only the switch.
        let worked = [user("u2", "fix the build"), agent("a2"), limit("limit")];
        assert_eq!(rejected_prompt(&worked, "limit"), None);
        // Something was sent since, or the notice is gone, or no message precedes it.
        let mut sent_since = quiet.to_vec();
        sent_since.push(user("u3", "never mind"));
        assert_eq!(rejected_prompt(&sent_since, "limit"), None);
        assert_eq!(rejected_prompt(&quiet, "other"), None);
        assert_eq!(rejected_prompt(&[limit("limit")], "limit"), None);
        // A message with an image is not its text alone.
        let mut with_image = user("u2", "what is this?");
        with_image.presentation.images.push(ChatImage {
            label: "Image 1".into(),
            source: ImageSource::Local {
                path: "/tmp/a.png".into(),
            },
        });
        assert_eq!(
            rejected_prompt(&[with_image, limit("limit")], "limit"),
            None
        );
        assert_eq!(
            rejected_prompt(&[user("u", "  "), limit("limit")], "limit"),
            None
        );
    }
}
