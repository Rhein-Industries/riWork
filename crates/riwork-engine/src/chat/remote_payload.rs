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
        for field in ["turn_id", "started_at", "ended_at"] {
            if let Some(value) = event["item"].get(field) {
                out[field] = value.clone();
            }
        }
        if let Some(parent) = event["item"]["presentation"].get("parent_id") {
            out["parent_id"] = parent.clone();
        }
        if let Some(kind) = event["item"]["presentation"]["semantics"].get("kind") {
            out["tool_kind"] = kind.clone();
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
    for field in ["turn_id", "started_at", "ended_at"] {
        if let Some(value) = item.get(field) {
            out[field] = value.clone();
        }
    }
    // Preserve the grouping discriminator and parentage even when output cannot
    // travel. Excerpts and other potentially large hints are best effort.
    let mut presentation = serde_json::Map::new();
    if let Some(parent) = item["presentation"].get("parent_id") {
        presentation.insert("parent_id".into(), parent.clone());
    }
    if let Some(kind) = item["presentation"]["semantics"].get("kind") {
        presentation.insert("semantics".into(), json!({"kind":kind}));
    }
    if !presentation.is_empty() {
        out["presentation"] = Value::Object(presentation);
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
            .filter(|(k, _)| !matches!(k.as_str(), "items" | "controls" | "requests"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    single["items"] = json!([]);
    single["controls"] = json!([]);
    if result.get("requests").is_some() {
        single["requests"] = json!([]);
    }
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
    single["controls"] = json!([]);
    if let Some(requests) = result.get_mut("requests").and_then(Value::as_array_mut) {
        for request in requests {
            if request.to_string().len() > payload_budget {
                cut_strings(request, 512);
            }
        }
    }
    // Seal the whole only once per pass. Ordinary rows paginate rather than elide.
    loop {
        if fits(&result)? {
            return Ok(Some(result));
        }
        let rows = result["items"].as_array().unwrap();
        if paginate && rows.len() > 1 {
            let preferred_cut = rows.len() / 2;
            let mut cut = preferred_cut;
            let mut boundary = snapshot_turn_boundary(&result, cut);
            loop {
                boundary = boundary.min(snapshot_turn_boundary(&result, cut));
                let previous_cut = cut;
                while cut > 0 && rows[cut - 1]["order"].as_u64().unwrap_or(0) >= boundary {
                    cut -= 1;
                }
                if cut == previous_cut {
                    break;
                }
            }
            // One oversized turn has to travel in partial pages.
            let forced_partial = cut == 0;
            if cut == 0 {
                cut = preferred_cut;
            }
            let last_dropped = rows[cut - 1]["order"].as_u64().unwrap_or(0);
            boundary = boundary.min(snapshot_turn_boundary(&result, cut));
            let partial_start = forced_partial
                || last_dropped >= boundary
                || (cut..rows.len())
                    .any(|index| snapshot_turn_boundary(&result, index) <= last_dropped);
            // Never hide a dropped row behind a turn/request boundary. In a
            // partial turn, the exact first kept order is the only safe cursor.
            let before = if last_dropped >= boundary {
                rows[cut]["order"].as_u64().unwrap_or(0)
            } else {
                boundary
            };
            result["items"].as_array_mut().unwrap().drain(..cut);
            result["provisional"] =
                json!(result["provisional"].as_bool().unwrap_or(false) || partial_start);
            if let Some(requests) = result.get_mut("requests").and_then(Value::as_array_mut) {
                requests.retain(|request| {
                    request["requested_seq"]
                        .as_u64()
                        .is_some_and(|seq| seq >= before)
                });
            }
            result["before"] = json!(before);
            result["more"] = json!(true);
            retain_snapshot_turn_metadata(&mut result);
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

fn snapshot_turn_boundary(snapshot: &Value, index: usize) -> u64 {
    let rows = snapshot["items"].as_array().unwrap();
    let row = &rows[index];
    let order = row["order"].as_u64().unwrap_or(0);
    let Some(turn) = row["item"]["turn_id"].as_str() else {
        return order;
    };
    let item_start = rows
        .iter()
        .filter(|row| row["item"]["turn_id"].as_str() == Some(turn))
        .filter_map(|row| row["order"].as_u64())
        .min()
        .unwrap_or(order);
    let request_start = snapshot
        .get("requests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|request| request["turn_id"].as_str() == Some(turn))
        .filter_map(|request| request["requested_seq"].as_u64())
        .min()
        .unwrap_or(order);
    snapshot["turn_starts"][turn]
        .as_u64()
        .unwrap_or(order)
        .min(item_start)
        .min(request_start)
        .min(order)
}

fn retain_snapshot_turn_metadata(snapshot: &mut Value) {
    let turns: std::collections::HashSet<String> = snapshot["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["item"]["turn_id"].as_str())
        .chain(
            snapshot
                .get("requests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|request| request["turn_id"].as_str()),
        )
        .chain(
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|control| control["event"] == "turn_started")
                .filter_map(|control| control["turn_id"].as_str()),
        )
        .map(str::to_owned)
        .collect();
    for key in ["turn_timing", "turn_starts"] {
        if let Some(map) = snapshot.get_mut(key).and_then(Value::as_object_mut) {
            map.retain(|turn, _| turns.contains(turn));
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

#[cfg(test)]
mod tool_metadata_tests {
    use super::*;

    #[test]
    fn elided_snapshot_items_keep_timing_parentage_and_tool_discriminator() {
        let item = json!({"id":"read","turn_id":"turn","status":"completed",
            "started_at":100,"ended_at":150,
            "presentation":{"parent_id":"worker","semantics":{"kind":"read","excerpt":"large"}},
            "body":{"type":"tool_call","tool":"Read","output":"large"}});
        let elided = elide_item(&item);
        assert_eq!(elided["started_at"], 100);
        assert_eq!(elided["ended_at"], 150);
        assert_eq!(elided["presentation"]["parent_id"], "worker");
        assert_eq!(elided["presentation"]["semantics"]["kind"], "read");
        assert!(elided["presentation"]["semantics"].get("excerpt").is_none());
    }

    #[test]
    fn transport_pagination_keeps_turns_and_request_records_together() {
        let items: Vec<_> = [2, 3, 20, 21].into_iter().enumerate().map(|(index, order)| {
            json!({"order":order,"item":{"id":order.to_string(),"turn_id":if index < 2 {"a"} else {"b"},
                "body":{"type":"agent_message","text":"x".repeat(3000)}}})
        }).collect();
        let snapshot = json!({"items":items,"controls":[],"before":1,"more":false,
            "requests":[{"request_id":"old","turn_id":"a","requested_seq":1},
                {"request_id":"kept","turn_id":"b","requested_seq":19}]});
        let result = fit_snapshot(snapshot, 7500, true, |value| {
            Ok::<_, ()>(value.to_string().len() <= 7500)
        })
        .unwrap()
        .unwrap();
        assert_eq!(result["items"].as_array().unwrap().len(), 2);
        assert_eq!(result["before"], 19);
        assert_eq!(result["provisional"], false);
        assert_eq!(result["requests"].as_array().unwrap().len(), 1);
        assert_eq!(result["requests"][0]["request_id"], "kept");
    }

    #[test]
    fn transport_partial_turn_cursor_never_skips_interleaved_rows() {
        for interleaved_turn in [Value::Null, json!("other")] {
            for with_turn_starts in [false, true] {
                let items: Vec<_> = [
                    (2, "a1", json!("t")),
                    (3, "n", interleaved_turn.clone()),
                    (4, "a2", json!("t")),
                    (5, "a3", json!("t")),
                ]
                .into_iter()
                .map(|(order, id, turn)| {
                    json!({"order":order,"item":{"id":id,"turn_id":turn,
                        "body":{"type":"agent_message","text":id}}})
                })
                .collect();
                let mut snapshot = json!({"items":items,"controls":[],"before":1,"more":false,
                    "requests":[{"request_id":"r","turn_id":"t","requested_seq":1}],
                    "turn_timing":{"t":{"started_at":100,"ended_at":500},"unrelated":{"started_at":1}}});
                if with_turn_starts {
                    snapshot["turn_starts"] = json!({"t":1,"other":3,"unrelated":0});
                }
                let original = snapshot.clone();
                let result = fit_snapshot(snapshot, usize::MAX, true, |value| {
                    Ok::<_, ()>(value["items"].as_array().unwrap().len() <= 2)
                })
                .unwrap()
                .unwrap();
                assert_eq!(result["items"].as_array().unwrap().len(), 2);
                assert_eq!(result["before"], 4);
                assert_eq!(result["provisional"], true);
                assert_eq!(result["more"], true);
                let before = result["before"].as_u64().unwrap();
                for row in original["items"].as_array().unwrap() {
                    let kept = result["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|kept| kept["item"]["id"] == row["item"]["id"]);
                    assert_eq!(kept, row["order"].as_u64().unwrap() >= before);
                }
                assert_eq!(result["requests"].as_array().unwrap().len(), 0);
                assert_eq!(result["turn_timing"]["t"]["ended_at"], 500);
                assert!(result["turn_timing"].get("unrelated").is_none());
                if with_turn_starts {
                    assert_eq!(result["turn_starts"], json!({"t":1}));
                }
            }
        }
    }

    #[test]
    fn transport_whole_turn_cut_keeps_notices_and_the_original_turn_boundary() {
        let snapshot = json!({"items":[
            {"order":2,"item":{"id":"old","turn_id":"a"}},
            {"order":19,"item":{"id":"notice"}},
            {"order":20,"item":{"id":"b1","turn_id":"b"}},
            {"order":21,"item":{"id":"b2","turn_id":"b"}}
        ],"controls":[],"before":1,"more":false,"turn_starts":{"a":1,"b":10},
        "requests":[{"request_id":"b","turn_id":"b","requested_seq":10}]});
        let result = fit_snapshot(snapshot, usize::MAX, true, |value| {
            Ok::<_, ()>(value["items"].as_array().unwrap().len() <= 3)
        })
        .unwrap()
        .unwrap();
        assert_eq!(result["items"].as_array().unwrap().len(), 3);
        assert_eq!(result["items"][0]["item"]["id"], "notice");
        assert_eq!(result["before"], 10);
        assert_eq!(result["provisional"], false);
        assert_eq!(result["requests"][0]["request_id"], "b");
    }
}
