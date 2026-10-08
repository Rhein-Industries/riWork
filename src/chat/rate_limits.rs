//! Provider quota decoding shared by the drivers, separate from notice policy.
use super::model::RateWindow;
use serde_json::Value;

pub(super) fn reset_seconds(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n < u64::MAX as f64)
        .map(|n| (if n > 1e11 { n / 1000.0 } else { n }) as u64)
}

fn percent(value: &Value, scale: f64) -> Option<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| (n * scale).clamp(0.0, 100.0))
}

fn claude_label(id: &str) -> &str {
    match id {
        "five_hour" => "5h",
        "seven_day" => "weekly",
        "seven_day_opus" => "weekly Opus",
        "seven_day_sonnet" => "weekly Sonnet",
        "overage" => "extra usage",
        _ => id,
    }
}

/// Claude updates can contain only the triggering window; retain other known windows.
pub(super) fn claude_windows(previous: &[RateWindow], info: &Value) -> Vec<RateWindow> {
    let mut windows = previous.to_vec();
    let mut merge = |id: &str, value: &Value| {
        let used = percent(&value["utilization"], 100.0);
        let reset = reset_seconds(&value["resetsAt"]);
        if let Some(window) = windows.iter_mut().find(|w| w.id == id) {
            if let Some(used) = used {
                window.used_percent = used;
            }
            if reset.is_some() {
                window.resets_at = reset;
            }
        } else if let Some(used) = used {
            windows.push(RateWindow {
                id: id.into(),
                label: claude_label(id).into(),
                used_percent: used,
                resets_at: reset,
                warn_at: 70.0,
            });
        }
    };
    if let Some(unified) = info["unifiedWindows"].as_object() {
        for (id, value) in unified {
            merge(id, value);
        }
    }
    if let Some(id) = info["rateLimitType"].as_str() {
        merge(id, info);
    }
    windows.sort_by(|a, b| a.id.cmp(&b.id));
    windows
}

/// Null in a sparse Codex update means "not supplied", including nested fields.
fn merge_non_null(target: &mut Value, patch: &Value) {
    if patch.is_null() {
        return;
    }
    if let Some(fields) = patch.as_object() {
        if !target.is_object() {
            *target = serde_json::json!({});
        }
        for (key, value) in fields {
            merge_non_null(&mut target[key], value);
        }
    } else {
        *target = patch.clone();
    }
}

#[derive(Default)]
pub(super) struct CodexRates {
    bucket: Value,
    pub windows: Vec<RateWindow>,
}

impl CodexRates {
    /// Prefer the account-wide codex bucket, then the existing bucket, then legacy
    /// rateLimits, or the first named bucket. IDs stay primary/secondary on the wire.
    pub fn update(&mut self, value: &Value) -> bool {
        let buckets = value["rateLimitsByLimitId"].as_object();
        let previous_id = self.bucket["limitId"].as_str();
        let named = buckets.and_then(|b| {
            b.get_key_value("codex")
                .filter(|(_, v)| v.is_object())
                .or_else(|| {
                    previous_id
                        .and_then(|id| b.get_key_value(id))
                        .filter(|(_, v)| v.is_object())
                })
        });
        let (id, bucket) = named
            .map(|(id, v)| (Some(id.as_str()), v))
            .or_else(|| {
                value
                    .get("rateLimits")
                    .filter(|b| b.is_object())
                    .map(|b| (b["limitId"].as_str(), b))
            })
            .or_else(|| {
                buckets
                    .and_then(|b| b.iter().find(|(_, v)| v.is_object()))
                    .map(|(id, v)| (Some(id.as_str()), v))
            })
            .unwrap_or((value["limitId"].as_str(), value));
        if id.is_some() && previous_id.is_some() && id != previous_id {
            self.bucket = serde_json::json!({});
        }
        merge_non_null(&mut self.bucket, bucket);
        if let Some(id) = id {
            self.bucket["limitId"] = Value::String(id.to_owned());
        }
        for key in ["planType", "rateLimitReachedType"] {
            if let Some(patch) = value.get(key) {
                merge_non_null(&mut self.bucket[key], patch);
            }
        }
        let plan = self.bucket["planType"].as_str();
        let mut windows = Vec::new();
        for id in ["primary", "secondary"] {
            let window = &self.bucket[id];
            let Some(used_percent) = percent(&window["usedPercent"], 1.0) else {
                continue;
            };
            let minutes = window["windowDurationMins"].as_u64();
            let label = match minutes {
                Some(300) => "5h".into(),
                Some(10080) => "weekly".into(),
                Some(n) if n >= 1440 && n.is_multiple_of(1440) => format!("{}d", n / 1440),
                Some(n) => format!("{}h", n as f64 / 60.0),
                None => id.into(),
            };
            let warn_at = if minutes == Some(300) && matches!(plan, Some("plus" | "team")) {
                50.0
            } else {
                75.0
            };
            windows.push(RateWindow {
                id: id.into(),
                label,
                used_percent,
                resets_at: reset_seconds(&window["resetsAt"]),
                warn_at,
            });
        }
        if windows == self.windows {
            return false;
        }
        self.windows = windows;
        true
    }

    pub fn blocking_reset(&self) -> Option<u64> {
        self.windows
            .iter()
            .filter(|w| w.used_percent >= 100.0)
            .filter_map(|w| w.resets_at)
            .min()
            .or_else(|| self.windows.iter().filter_map(|w| w.resets_at).min())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_sparse_windows_and_plan_thresholds_use_the_named_bucket_without_duplicates() {
        let mut rates = CodexRates::default();
        assert!(rates.update(&json!({"rateLimitsByLimitId":{"codex":{
            "primary":{"usedPercent":49,"windowDurationMins":300,"resetsAt":1767225600000u64},
            "secondary":{"usedPercent":76,"windowDurationMins":10080,"resetsAt":1767300000},
            "planType":"plus","rateLimitReachedType":"none"
        }}, "rateLimits":{"primary":{"usedPercent":99}}})));
        assert_eq!(rates.windows.len(), 2);
        assert_eq!(rates.windows[0].warn_at, 50.0);
        assert!(rates.update(&json!({"rateLimits":{"primary":{"usedPercent":51,"resetsAt":null},"secondary":null,"planType":null}})));
        assert_eq!(
            (rates.windows[0].used_percent, rates.windows[0].resets_at),
            (51.0, Some(1767225600))
        );
        assert_eq!(rates.windows[1].used_percent, 76.0);
        assert!(!rates.update(&json!({"rateLimits":{"primary":null,"planType":null}})));
        for plan in ["team", "plus", "pro", "free"] {
            rates.update(&json!({"planType":plan}));
            assert_eq!(
                rates.windows[0].warn_at,
                if matches!(plan, "team" | "plus") {
                    50.0
                } else {
                    75.0
                }
            );
        }
        rates.update(&json!({"primary":{"usedPercent":120,"windowDurationMins":1440},"secondary":{"usedPercent":-1,"windowDurationMins":120}}));
        assert_eq!(
            (
                rates.windows[0].label.as_str(),
                rates.windows[0].used_percent
            ),
            ("1d", 100.0)
        );
        assert_eq!(
            (
                rates.windows[1].label.as_str(),
                rates.windows[1].used_percent
            ),
            ("2h", 0.0)
        );
    }
}
