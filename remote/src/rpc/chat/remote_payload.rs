//! Response-only recovery; never changes the durable chat log.
use serde_json::{Value, json};

pub const MIN_CUT: usize = 128;
pub const TRUNCATION_NOTE: &str =
    "This message is too long to show here. Full text is on your Mac.";

fn identity_key(key: &str) -> bool {
    key == "id" || key.ends_with("_id") || matches!(key, "path" | "url" | "name")
}
fn binary_key(key: &str) -> bool {
    key.to_ascii_lowercase().contains("base64") || matches!(key, "image_data" | "binary_data")
}
fn omitted_binary() -> Value {
    json!({"kind":"unavailable", "reason":"Image omitted to fit the remote response limit"})
}

pub fn cut_strings(value: &mut Value, cap: usize) {
    if let Some(fields) = value.as_object() {
        let encoded = fields.get("encoding").and_then(Value::as_str) == Some("base64");
        let image = fields.get("kind").and_then(Value::as_str) == Some("data")
            || fields.get("type").and_then(Value::as_str) == Some("image")
            || fields.contains_key("mime");
        if fields.iter().any(|(k, v)| {
            (binary_key(k)
                || ((encoded || image) && matches!(k.as_str(), "data" | "value" | "content")))
                && v.as_str().is_some_and(|s| s.len() > cap)
        }) {
            // Omit the entire encoded source, retaining nested identity values.
            let mut omitted = identities(value);
            omitted["kind"] = json!("unavailable");
            omitted["reason"] = omitted_binary()["reason"].clone();
            *value = omitted;
            return;
        }
    }
    match value {
        Value::String(s) if s.len() > cap && s.starts_with("data:") && s.contains(";base64,") => {
            *value = omitted_binary()
        }
        Value::String(s) if s.len() > cap => {
            let mut end = cap;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push('…');
        }
        Value::Array(a) => a.iter_mut().for_each(|v| cut_strings(v, cap)),
        Value::Object(o) => o
            .iter_mut()
            .filter(|(k, _)| !identity_key(k))
            .for_each(|(_, v)| cut_strings(v, cap)),
        _ => {}
    }
}

/// Cut nested output/diffs/input and presentation attachments/images, preserving identity.
pub fn shorten_body(event: &mut Value, cap: usize) -> bool {
    shorten_body_at_min(event, cap, MIN_CUT)
}

pub fn shorten_body_at_min(event: &mut Value, cap: usize, minimum: usize) -> bool {
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
                if cap == minimum {
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
        let mut out =
            json!({"event":"item_elided","of":event["event"],"reason":"too_large","bytes":bytes});
        if let Some(id) = event["item"]["id"]
            .as_str()
            .or_else(|| event["item_id"].as_str())
        {
            out["item_id"] = json!(id);
        }
        if let Some(status) = event["item"].get("status") {
            out["status"] = status.clone();
        } else if event["event"] == "item_completed" {
            out["status"] = json!("completed");
        }
        if let Some(kind) = event["item"]["body"]["type"]
            .as_str()
            .or_else(|| event["kind"].as_str())
        {
            out["kind"] = json!(kind);
        }
        out
    } else {
        let mut out = identities(event);
        out["event"] = json!("control_elided");
        out["of"] = event["event"].clone();
        out["reason"] = json!("too_large");
        out["bytes"] = json!(bytes);
        out
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
            json!({"event":"item_elided","item_id":"tool","of":"item_delta","reason":"too_large",
            "bytes":serde_json::to_vec(&event).unwrap().len()})
        );
    }
}

/// Recover individually oversized payloads and page ordinary snapshot rows. The
/// sizing sample contains metadata plus one row/control, never the whole snapshot.
/// `fits` may use raw CLI JSON or the connector's real sealed/compressed frame.
pub fn fit_snapshot<E>(
    mut result: Value,
    payload_budget: usize,
    paginate: bool,
    fits: impl Fn(&Value) -> Result<bool, E>,
) -> Result<Option<Value>, E> {
    if fits(&result)? {
        return Ok(Some(result));
    }
    let mut single = Value::Object(
        result
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.as_str() != "items" && k.as_str() != "controls")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    single["items"] = json!([]);
    single["controls"] = json!([]);
    for row in result["items"].as_array_mut().unwrap() {
        // The conservative fast path avoids encoding/compressing small rows.
        if row.to_string().len() <= payload_budget {
            continue;
        }
        single["items"] = json!([row]);
        if fits(&single)? {
            continue;
        }
        let original = row["item"].clone();
        let event = json!({"event":"item_completed","item":original});
        let mut recovered = None;
        let mut cap = 64 * 1024;
        while cap >= MIN_CUT {
            let mut shrunk = event.clone();
            shorten_body(&mut shrunk, cap);
            single["items"][0]["item"] = shrunk["item"].clone();
            if single["items"][0].to_string().len() <= payload_budget {
                recovered = Some(shrunk["item"].clone());
                break;
            }
            cap /= 2;
        }
        row["item"] = recovered.unwrap_or_else(|| elide_item(&original));
    }
    single["items"] = json!([]);
    for control in result["controls"].as_array_mut().unwrap() {
        if control.to_string().len() <= payload_budget {
            continue;
        }
        single["controls"] = json!([control]);
        if !fits(&single)? {
            *control = elide_event(control);
        }
    }
    // Seal the whole only once per pass. Ordinary rows paginate rather than elide.
    loop {
        if fits(&result)? {
            return Ok(Some(result));
        }
        let rows = result["items"].as_array_mut().unwrap();
        if paginate && rows.len() > 1 {
            rows.drain(..rows.len() / 2);
            result["before"] = result["items"][0]["order"].clone();
            result["more"] = json!(true);
            continue;
        }
        // An aggregate of small controls has no history pagination. Replace a batch
        // of the largest controls; estimate bytes without repeatedly sealing the page.
        let mut total = result.to_string().len();
        let controls = result["controls"].as_array_mut().unwrap();
        let mut sizes: Vec<_> = controls
            .iter()
            .enumerate()
            .filter(|(_, v)| v["event"] != "control_elided" && v["event"] != "item_elided")
            .map(|(i, v)| (i, v.to_string().len()))
            .collect();
        sizes.sort_unstable_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        let mut changed = false;
        for (index, bytes) in sizes {
            controls[index] = elide_event(&controls[index]);
            total = total.saturating_sub(bytes) + controls[index].to_string().len();
            changed = true;
            if total <= payload_budget {
                break;
            }
        }
        if !changed {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn nested_identity_fields_are_exact_and_encoded_payloads_are_omitted() {
        let long = "identity".repeat(1000);
        let mut value = json!({"nested":{"id":long,"item_id":long,"call_id":long,"request_id":long,
            "path":long,"url":long,"name":long,"text":"text".repeat(1000)},
            "generic":{"base64":"a".repeat(2000),"call_id":"call"},
            "encoded":{"encoding":"base64","value":"a".repeat(2000)},
            "image":{"type":"image","data":"a".repeat(2000)},
            "uri":"data:image/png;base64,".to_owned() + &"a".repeat(2000)});
        cut_strings(&mut value, 128);
        for field in [
            "id",
            "item_id",
            "call_id",
            "request_id",
            "path",
            "url",
            "name",
        ] {
            assert_eq!(value["nested"][field], long);
        }
        assert!(value["nested"]["text"].as_str().unwrap().ends_with('…'));
        for field in ["generic", "encoded", "image", "uri"] {
            assert_eq!(value[field]["kind"], "unavailable");
        }
        assert_eq!(value["generic"]["call_id"], "call");
        assert!(value["generic"].get("base64").is_none());
    }

    #[test]
    fn item_completion_elision_retains_original_event_status_and_optional_identity() {
        let completed = json!({"event":"item_completed","item":{"id":"tool","status":"failed","body":{"type":"tool_call"}}});
        let placeholder = elide_event(&completed);
        assert_eq!(placeholder["of"], "item_completed");
        assert_eq!(placeholder["status"], "failed");
        assert_eq!(placeholder["item_id"], "tool");
        let anonymous =
            elide_event(&json!({"event":"item_delta","delta":{"kind":"text","text":"x"}}));
        assert!(anonymous.get("item_id").is_none());
        assert!(anonymous.get("status").is_none());
    }

    #[test]
    fn snapshot_sizing_seals_per_pass_instead_of_per_small_row() {
        let items: Vec<_> = (1..=100)
            .map(|order| {
                json!({"order":order,"item":{"id":order.to_string(),
            "status":"completed","body":{"type":"agent_message","text":"x".repeat(4000)}}})
            })
            .collect();
        let snapshot = json!({"items":items,"controls":[],"before":1,"more":false});
        let count = std::cell::Cell::new(0);
        let page = fit_snapshot(snapshot, 128_000, true, |value| {
            count.set(count.get() + 1);
            Ok::<_, ()>(value.to_string().len() < 130_000)
        })
        .unwrap()
        .unwrap();
        assert!(count.get() < 10, "{}", count.get());
        assert!(page["items"].as_array().unwrap().len() < 100);
        assert!(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["item"].get("elided").is_none())
        );
    }
}
