//! The models and reasoning efforts a chat is offered, per provider: what the chat tab's
//! Model and Effort pop-ups list, and what `riwork chat options` prints for the phone. Any
//! other model name may still be typed; the host passes it on as it is, so a name or a level
//! the installed CLI does not know is the provider's to refuse.

use super::model::Provider;
use serde_json::{Value, json};

/// The reasoning efforts offered for a provider.
pub fn efforts(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &["low", "medium", "high", "xhigh"],
        Provider::Claude => &["low", "medium", "high", "xhigh", "max"],
    }
}

/// Models offered by name. Claude's aliases keep working as models change; Codex model names
/// change often enough that only a typed name is offered.
pub fn model_suggestions(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => &[],
        Provider::Claude => &["opus", "sonnet", "haiku"],
    }
}

/// What `riwork chat options --json` prints: `{"providers":{"codex":{"models":[..],
/// "efforts":[..]},"claude":{..}}}`, in the order the pop-ups list them.
pub fn options_json() -> Value {
    let entry = |provider| {
        json!({
            "models": model_suggestions(provider),
            "efforts": efforts(provider),
        })
    };
    json!({
        "providers": {
            "codex": entry(Provider::Codex),
            "claude": entry(Provider::Claude),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_are_the_lists_of_each_provider() {
        let options = options_json();
        assert_eq!(options["providers"]["codex"]["models"], json!([]));
        assert_eq!(
            options["providers"]["codex"]["efforts"],
            json!(["low", "medium", "high", "xhigh"])
        );
        assert_eq!(
            options["providers"]["claude"]["models"],
            json!(["opus", "sonnet", "haiku"])
        );
        assert_eq!(
            options["providers"]["claude"]["efforts"],
            json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(options.as_object().unwrap().len(), 1);
    }
}
