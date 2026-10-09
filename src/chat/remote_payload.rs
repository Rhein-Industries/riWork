//! Response-only recovery; never changes the durable chat log.
use serde_json::{Value, json};

pub const MIN_CUT: usize = 128;
pub const TRUNCATION_NOTE: &str =
    "This message is too long to show here. Full text is on your Mac.";

pub fn cut_strings(value: &mut Value, cap: usize) {
    if value["kind"] == "data" && value["base64"].as_str().is_some_and(|s| s.len() > cap) {
        *value = json!({"kind":"unavailable", "reason":"Image omitted to fit the remote response limit"});
        return;
    }
    match value {
        Value::String(s) if s.len() > cap => {
            let mut end = cap;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push('…');
        }
        Value::Array(a) => a.iter_mut().for_each(|v| cut_strings(v, cap)),
        Value::Object(o) => o.values_mut().for_each(|v| cut_strings(v, cap)),
        _ => {}
    }
}

/// Cut nested output/diffs/input and presentation attachments/images, preserving identity.
pub fn shorten_body(event: &mut Value, cap: usize) -> bool {
    match event["event"].as_str() {
        Some("item_started" | "item_completed") => {
            if let Some(item) = event.get_mut("item").and_then(Value::as_object_mut) {
                for key in [
                    "body",
                    "presentation",
                    "output",
                    "diff",
                    "attachments",
                    "images",
                ] {
                    if let Some(v) = item.get_mut(key) {
                        cut_strings(v, cap);
                    }
                }
                if cap == MIN_CUT {
                    item.insert(
                        "body".into(),
                        json!({"type":"agent_message","text":TRUNCATION_NOTE}),
                    );
                }
            }
            true
        }
        Some("item_delta") => {
            cut_strings(&mut event["delta"], cap);
            true
        }
        _ => false,
    }
}

/// Preserve identity at any object depth; omit payloads without presenting partial requests.
fn identities(value: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(fields) = value.as_object() {
        for (key, v) in fields {
            if (key == "id" || key.ends_with("_id") || key == "event") && v.is_string() {
                out.insert(key.clone(), v.clone());
            } else if v.is_object() {
                let nested = identities(v);
                if nested.as_object().is_some_and(|o| o.len() > 1) {
                    out.insert(key.clone(), nested);
                }
            }
        }
    }
    out.insert("elided".into(), json!(true));
    Value::Object(out)
}

pub fn elide_event(event: &Value) -> Value {
    let bytes = serde_json::to_vec(event).expect("JSON value").len();
    if matches!(
        event["event"].as_str(),
        Some("item_started" | "item_completed" | "item_delta")
    ) {
        let id = event["item"]["id"]
            .as_str()
            .or_else(|| event["item_id"].as_str())
            .unwrap_or("");
        let mut out =
            json!({"event":"item_elided","item_id":id,"reason":"too_large","bytes":bytes});
        if let Some(kind) = event["item"]["body"]["type"]
            .as_str()
            .or_else(|| event["kind"].as_str())
        {
            out["kind"] = json!(kind);
        }
        out
    } else {
        identities(event)
    }
}

/// Snapshots contain items rather than sequenced events. Keep old clients' Item schema.
pub fn elide_item(item: &Value) -> Value {
    let mut out = json!({"id":item["id"],"status":item["status"],
        "body":{"type":"agent_message","text":TRUNCATION_NOTE},
        "elided":elide_event(&json!({"event":"item_completed","item":item}))});
    out["elided"]["bytes"] = json!(serde_json::to_vec(item).expect("JSON value").len());
    if let Some(turn) = item.get("turn_id") {
        out["turn_id"] = turn.clone();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tool_diff_and_presentation_payloads_shrink_before_elision() {
        let mut event = json!({"event":"item_completed","item":{"id":"stable","turn_id":"turn",
            "body":{"type":"tool_call","output":{"nested":"x".repeat(5000)},"diff":[{"text":"y".repeat(5000)}]},
            "presentation":{"attachments":[{"preview":"z".repeat(5000)}],
                "images":[{"source":{"kind":"data","mime":"image/png","base64":"a".repeat(2 * 1024 * 1024)}}]}}});
        assert!(shorten_body(&mut event, 1024));
        assert_eq!(event["item"]["id"], "stable");
        assert_eq!(event["item"]["turn_id"], "turn");
        assert!(
            event["item"]["body"]["output"]["nested"]
                .as_str()
                .unwrap()
                .ends_with('…')
        );
        assert!(
            event["item"]["body"]["diff"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with('…')
        );
        assert!(
            event["item"]["presentation"]["attachments"][0]["preview"]
                .as_str()
                .unwrap()
                .ends_with('…')
        );
        let image = &event["item"]["presentation"]["images"][0]["source"];
        assert_eq!(image["kind"], "unavailable");
        assert!(image.get("base64").is_none());
    }

    #[test]
    fn delta_elision_omits_unknown_kind_and_counts_original_event_bytes() {
        let event = json!({"event":"item_delta","item_id":"tool","delta":{"kind":"output","text":"x".repeat(2 * 1024 * 1024)}});
        assert_eq!(
            elide_event(&event),
            json!({"event":"item_elided","item_id":"tool","reason":"too_large",
            "bytes":serde_json::to_vec(&event).unwrap().len()})
        );
    }
}
