use super::*;
use crate::anthropic_key::MemoryStore;
use std::sync::Mutex;

const NOW: u64 = 1_800_000_000;
const CHAT: &str = "11111111-1111-4111-8111-111111111111";
const KEY: &str = "sk-ant-fixture-never-network";

struct Sandbox(std::path::PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "riwork-dashboard-intelligence-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn cache(&self) -> std::path::PathBuf {
        self.0.join(CACHE_DIRECTORY).join("summary.json")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Stub {
    calls: Mutex<Vec<Value>>,
    response: Mutex<Result<Value, &'static str>>,
}
impl Stub {
    fn new(seq: u64) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response: Mutex::new(Ok(response(seq))),
        }
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn set(&self, value: Result<Value, &'static str>) {
        *self.response.lock().unwrap() = value;
    }
}
impl Completion for Stub {
    fn complete(&self, key: &str, request: &Value) -> Result<Value, &'static str> {
        assert!(key == KEY);
        self.calls.lock().unwrap().push(request.clone());
        self.response.lock().unwrap().clone()
    }
}
fn wire(seq: u64) -> Value {
    json!({"v":1,"generated_at":NOW,"totals":{"projects":2,"agents":{"working":1}},"decision_sources":{"state":"read","sampled_at":NOW,"chats_total":1,"chats_complete":1},
        "recent_activity":[{"session":{"agent":"Codex","title":"Fix build","target":{"type":"chat","id":CHAT}},"source":{"chat_id":CHAT,"event_seq":seq,"event_id":format!("chat:{CHAT}:seq:{seq}")},"observed_at":NOW,"log_updated_at":NOW,"kind":"tool","text":"Build succeeded","status":"completed"}]})
}
fn response(seq: u64) -> Value {
    json!({"stop_reason":"end_turn","content":[{"type":"text","text":json!({"summary":"The build succeeded.","highlights":[{"text":"Build verification passed.","source_event_ids":[format!("chat:{CHAT}:seq:{seq}")]}],"followups":[]}).to_string()}]})
}
fn invoke(sandbox: &Sandbox, wire: &Value, now: u64, keys: &dyn KeyStore, stub: &Stub) -> Value {
    enrich_with(&sandbox.0, wire, true, now, keys, stub)
}

#[test]
fn disabled_and_empty_evidence_do_not_read_keys_create_cache_or_call_inference() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    assert_eq!(
        enrich_with(&home.0, &wire(1), false, NOW, &keys, &stub)["state"],
        "disabled"
    );
    assert_eq!(
        invoke(
            &home,
            &json!({"v":1,"totals":{"projects":1}}),
            NOW,
            &keys,
            &stub
        )["state"],
        "no_evidence"
    );
    assert_eq!(keys.lookups(), 0);
    assert_eq!(stub.count(), 0);
    assert!(!home.0.join(CACHE_DIRECTORY).exists());
}

#[test]
fn missing_or_unreadable_key_has_a_safe_message_and_does_not_call_api() {
    let home = Sandbox::new();
    let keys = MemoryStore::default();
    let stub = Stub::new(1);
    assert_eq!(
        invoke(&home, &wire(1), NOW, &keys, &stub)["state"],
        "not_configured"
    );
    // The key is not asked for again within the interval, and the state stays explained.
    let waiting = invoke(&home, &wire(2), NOW + 5, &keys, &stub);
    assert_eq!(waiting["state"], "not_configured");
    assert!(
        waiting["error"]
            .as_str()
            .unwrap()
            .contains("Save an Anthropic API key")
    );
    assert_eq!(keys.lookups(), 1);
    keys.fail_reads(Some("sensitive-keychain-debug-value"));
    let result = invoke(&home, &wire(1), NOW + 60, &keys, &stub);
    assert_eq!(result["state"], "not_configured");
    assert!(
        !result
            .to_string()
            .contains("sensitive-keychain-debug-value")
    );
    assert_eq!(stub.count(), 0);
    let saved = fs::read_to_string(home.cache()).unwrap();
    assert!(!saved.contains("sensitive-keychain-debug-value"));
    let cache = read_cache(&home.cache()).unwrap().unwrap();
    assert_eq!(cache.requests_today, 0, "a key check spends no budget");
}

#[test]
fn unchanged_evidence_deduplicates_across_dashboard_reads_and_changing_sample_times() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    let first = invoke(&home, &wire(1), NOW, &keys, &stub);
    assert_eq!(first["state"], "ready");
    assert_eq!(first["stale"], false);
    let mut later = wire(1);
    later["generated_at"] = (NOW + 600).into();
    later["decision_sources"]["sampled_at"] = (NOW + 600).into();
    later["recent_activity"][0]["observed_at"] = (NOW + 600).into();
    later["recent_activity"][0]["log_updated_at"] = (NOW + 600).into();
    assert_eq!(invoke(&home, &later, NOW + 600, &keys, &stub), first);
    assert_eq!(stub.count(), 1);
    assert_eq!(keys.lookups(), 1);
    let request = &stub.calls.lock().unwrap()[0];
    assert_eq!(request["model"], MODEL);
    assert_eq!(request["output_config"]["effort"], "low");
    assert_eq!(request["thinking"]["type"], "disabled");
    assert_eq!(request["output_config"]["format"]["type"], "json_schema");
    assert!(request.get("tools").is_none());
    assert!(request.get("temperature").is_none());
    let saved = fs::read_to_string(home.cache()).unwrap();
    assert!(!saved.contains(KEY));
    assert!(
        !saved.contains("Build succeeded"),
        "raw evidence is not persisted"
    );
    assert_eq!(
        fs::metadata(home.cache()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(home.0.join(CACHE_DIRECTORY))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn changing_evidence_keeps_the_previous_summary_explicitly_stale_until_the_rate_window_opens() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    invoke(&home, &wire(1), NOW, &keys, &stub);
    let stale = invoke(&home, &wire(2), NOW + 5, &keys, &stub);
    assert_eq!(stale["state"], "rate_limited");
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["summary"], "The build succeeded.");
    assert_eq!(stub.count(), 1);
    stub.set(Ok(response(2)));
    let fresh = invoke(&home, &wire(2), NOW + 60, &keys, &stub);
    assert_eq!(fresh["state"], "ready");
    assert_eq!(fresh["stale"], false);
    assert_eq!(stub.count(), 2);
}

#[test]
fn failed_and_uncited_results_preserve_raw_facts_and_never_replace_a_valid_summary() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    let original = wire(1);
    invoke(&home, &original, NOW, &keys, &stub);
    stub.set(Err("Haiku could not be reached."));
    let unavailable = invoke(&home, &wire(2), NOW + 60, &keys, &stub);
    assert_eq!(unavailable["state"], "unavailable");
    assert_eq!(unavailable["stale"], true);
    assert_eq!(unavailable["summary"], "The build succeeded.");
    stub.set(Ok(response(9999)));
    let uncited = invoke(&home, &wire(2), NOW + 120, &keys, &stub);
    assert_eq!(uncited["state"], "unavailable");
    assert!(
        uncited["error"]
            .as_str()
            .unwrap()
            .contains("outside this dashboard")
    );
    assert_eq!(
        read_cache(&home.cache()).unwrap().unwrap().fingerprint,
        fingerprint(&evidence(&original).unwrap())
    );
    assert_eq!(
        original,
        wire(1),
        "enrichment never changes authoritative state or decisions"
    );
}

#[test]
fn the_daily_limit_is_reserved_on_disk_before_requests_and_resets_next_day() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    invoke(&home, &wire(1), NOW, &keys, &stub);
    let mut cache = read_cache(&home.cache()).unwrap().unwrap();
    assert_eq!(cache.requests_today, 1);
    cache.requests_today = DAILY_LIMIT;
    write_cache(&home.cache(), &cache).unwrap();
    assert_eq!(
        invoke(&home, &wire(2), NOW + 60, &keys, &stub)["state"],
        "rate_limited"
    );
    assert_eq!(stub.count(), 1);
    stub.set(Ok(response(2)));
    assert_eq!(
        invoke(&home, &wire(2), NOW + 86_400, &keys, &stub)["state"],
        "ready"
    );
    assert_eq!(stub.count(), 2);
    assert_eq!(
        read_cache(&home.cache()).unwrap().unwrap().requests_today,
        1
    );
    // A clock set back to the previous day grants no new budget.
    let mut cache = read_cache(&home.cache()).unwrap().unwrap();
    cache.requests_today = DAILY_LIMIT;
    write_cache(&home.cache(), &cache).unwrap();
    assert_eq!(
        invoke(&home, &wire(3), NOW + 120, &keys, &stub)["state"],
        "rate_limited"
    );
    assert_eq!(stub.count(), 2);
}

#[test]
fn another_process_holding_the_lock_returns_immediately_without_inference() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    invoke(&home, &wire(1), NOW, &keys, &stub);
    let lock = private_file(&home.0.join(CACHE_DIRECTORY).join("summary.lock"), false).unwrap();
    lock.lock_exclusive().unwrap();
    let result = invoke(&home, &wire(2), NOW + 60, &keys, &stub);
    assert_eq!(result["state"], "refreshing");
    assert_eq!(result["stale"], true);
    assert_eq!(stub.count(), 1);
}

#[test]
fn evidence_is_bounded_and_strips_action_authority_and_revalidation_timestamps() {
    let mut wire = wire(1);
    let source = wire["recent_activity"][0]["source"].clone();
    wire["decisions"] = json!([{"source":source,"kind":"approval","approval":{"title":"Allow build?","detail":"SECRET DETAIL","choices":["accept","decline"]},"observed_at":NOW}]);
    let mut rows = Vec::new();
    for index in 0..1000 {
        let mut row = wire["recent_activity"][0].clone();
        row["source"]["event_seq"] = index.into();
        row["source"]["event_id"] = format!("chat:{CHAT}:seq:{index}").into();
        row["text"] = "日本語".repeat(10000).into();
        rows.push(row);
    }
    wire["recent_activity"] = rows.into();
    let evidence = evidence(&wire).unwrap();
    let text = evidence.to_string();
    assert!(text.len() <= INPUT_BYTES);
    assert!(!text.contains("SECRET DETAIL"));
    assert!(!text.contains("choices"));
    assert!(!text.contains("observed_at"));
    assert!(!text.contains("sampled_at"));
    assert!(text.contains("Allow build?"));
    assert!(evidence["events"].as_array().unwrap().len() <= 32);
}

#[test]
fn refuses_truncated_oversized_unknown_fields_and_unattributed_outputs() {
    let ids = source_ids(&evidence(&wire(1)).unwrap());
    let mut truncated = response(1);
    truncated["stop_reason"] = "max_tokens".into();
    assert!(parse_response(&truncated, &ids).is_err());
    let mut raw: Value =
        serde_json::from_str(response(1)["content"][0]["text"].as_str().unwrap()).unwrap();
    raw["command"] = "approve_all".into();
    let mutated = |raw: &Value| json!({"stop_reason":"end_turn","content":[{"type":"text","text":raw.to_string()}]});
    assert!(parse_response(&mutated(&raw), &ids).is_err());
    raw.as_object_mut().unwrap().remove("command");
    raw["highlights"][0]["source_event_ids"] = json!([]);
    assert!(parse_response(&mutated(&raw), &ids).is_err());
    raw["highlights"] = json!([]);
    raw["summary"] = "x".repeat(601).into();
    assert!(parse_response(&mutated(&raw), &ids).is_err());
}

#[test]
fn corrupt_or_symlinked_cache_fails_closed_instead_of_resetting_spending() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    private_directory(&home.0.join(CACHE_DIRECTORY)).unwrap();
    fs::write(home.cache(), "not json").unwrap();
    assert_eq!(
        invoke(&home, &wire(1), NOW, &keys, &stub)["state"],
        "unavailable"
    );
    assert_eq!(stub.count(), 0);
    fs::remove_file(home.cache()).unwrap();
    let outside = home.0.join("outside.json");
    fs::write(&outside, "unchanged").unwrap();
    std::os::unix::fs::symlink(&outside, home.cache()).unwrap();
    assert_eq!(
        invoke(&home, &wire(1), NOW, &keys, &stub)["state"],
        "unavailable"
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
    assert_eq!(stub.count(), 0);
}

#[test]
fn unchanged_evidence_that_keeps_failing_is_retried_after_a_doubling_wait() {
    let home = Sandbox::new();
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    stub.set(Err("Haiku could not be reached."));
    assert_eq!(
        invoke(&home, &wire(1), NOW, &keys, &stub)["state"],
        "unavailable"
    );
    assert_eq!(
        invoke(&home, &wire(1), NOW + 60, &keys, &stub)["state"],
        "unavailable"
    );
    assert_eq!(stub.count(), 2);
    // A second failure of the same evidence waits two intervals, and still says why.
    let waiting = invoke(&home, &wire(1), NOW + 150, &keys, &stub);
    assert_eq!(waiting["state"], "unavailable");
    assert!(
        waiting["error"]
            .as_str()
            .unwrap()
            .contains("could not be reached")
    );
    assert_eq!(stub.count(), 2);
    invoke(&home, &wire(1), NOW + 180, &keys, &stub);
    assert_eq!(stub.count(), 3);
    invoke(&home, &wire(1), NOW + 180 + 239, &keys, &stub);
    assert_eq!(stub.count(), 3);
    // New evidence is asked about after the ordinary interval.
    stub.set(Ok(response(2)));
    assert_eq!(
        invoke(&home, &wire(2), NOW + 240, &keys, &stub)["state"],
        "ready"
    );
    assert_eq!(stub.count(), 4);
    let cache = read_cache(&home.cache()).unwrap().unwrap();
    assert_eq!((cache.failed, cache.failures), (None, 0));
}

#[test]
fn the_app_and_the_cli_share_a_summary_whatever_their_counts_and_event_order() {
    let mut a = wire(1);
    let mut second = a["recent_activity"][0].clone();
    second["source"]["event_seq"] = 2.into();
    second["source"]["event_id"] = format!("chat:{CHAT}:seq:2").into();
    second["text"] = "Tests running".into();
    a["recent_activity"].as_array_mut().unwrap().push(second);
    let mut b = a.clone();
    b["recent_activity"].as_array_mut().unwrap().reverse();
    b["totals"]["agents"]["working"] = 3.into();
    b["decision_sources"]["chats_read"] = 5.into();
    b["working"] = json!([{"session":{"agent":"Codex","title":"Fix build"}}]);
    let print = |wire: &Value| fingerprint(&evidence(wire).unwrap());
    assert_eq!(print(&a), print(&b));
    let mut changed = a.clone();
    changed["recent_activity"][0]["status"] = "failed".into();
    assert_ne!(print(&a), print(&changed));
    let mut partial = a.clone();
    partial["decision_sources"]["state"] = "partial".into();
    assert_ne!(print(&a), print(&partial));
}

#[test]
fn hidden_text_is_refused_and_repeated_items_are_kept_once() {
    let ids = source_ids(&evidence(&wire(1)).unwrap());
    let item = json!({"text":"Build passed.","source_event_ids":[format!("chat:{CHAT}:seq:1")]});
    let reply = |raw: Value| json!({"stop_reason":"end_turn","content":[{"type":"text","text":raw.to_string()}]});
    let summary = parse_response(
        &reply(json!({"summary":"Done.","highlights":[item, item],"followups":[item]})),
        &ids,
    )
    .unwrap();
    assert_eq!((summary.highlights.len(), summary.followups.len()), (1, 1));
    let spoofed = json!({"summary":"Done \u{202E}.","highlights":[],"followups":[]});
    assert!(parse_response(&reply(spoofed), &ids).is_err());
    assert_eq!(bounded("a\u{200B}b\u{2066}c", 10), "abc");
}

#[test]
fn a_saved_read_never_asks_keychain_or_haiku_and_says_when_a_refresh_is_due() {
    let home = Sandbox::new();
    let (first, due) = saved_with(&home.0, &wire(1), true, NOW);
    assert_eq!(first["state"], "refreshing");
    assert!(
        !home.0.join(CACHE_DIRECTORY).exists(),
        "a saved read writes nothing"
    );
    let keys = MemoryStore::with_key(KEY);
    let stub = Stub::new(1);
    // The refresh process summarizes exactly the evidence it was handed.
    let refreshed = summarize(&home.0, &due.unwrap(), NOW, &keys, &stub);
    assert_eq!(refreshed["state"], "ready");
    let (ready, due) = saved_with(&home.0, &wire(1), true, NOW + 30);
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["summary"], "The build succeeded.");
    assert!(due.is_none());
    let (waiting, due) = saved_with(&home.0, &wire(2), true, NOW + 30);
    assert_eq!(waiting["state"], "rate_limited");
    assert_eq!(waiting["stale"], true);
    assert!(due.is_none());
    assert!(saved_with(&home.0, &wire(2), true, NOW + 60).1.is_some());
    let (disabled, due) = saved_with(&home.0, &wire(1), false, NOW);
    assert_eq!(disabled["state"], "disabled");
    assert!(due.is_none());
    assert_eq!(stub.count(), 1);
    assert_eq!(keys.lookups(), 1);
}

#[test]
fn a_refresh_process_needs_the_setting_and_well_formed_evidence() {
    let home = Sandbox::new();
    let evidence = serde_json::to_vec(&evidence(&wire(1)).unwrap()).unwrap();
    refresh(&home.0, evidence.as_slice());
    assert!(!home.0.join(CACHE_DIRECTORY).exists(), "off by default");
    SettingsStore::open(&home.0)
        .unwrap()
        .update(|settings| settings.dashboard_haiku_summaries = true)
        .unwrap();
    refresh(&home.0, &b"not evidence"[..]);
    refresh(&home.0, &br#"{"events":[]}"#[..]);
    assert!(!home.0.join(CACHE_DIRECTORY).exists());
    // Test builds have no key and no network: the attempt stops at the key.
    refresh(&home.0, evidence.as_slice());
    let cache = read_cache(&home.cache()).unwrap().unwrap();
    assert_eq!(cache.failed.as_deref(), Some("not_configured"));
    assert_eq!(cache.requests_today, 0);
}
