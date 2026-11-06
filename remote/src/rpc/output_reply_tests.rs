use super::*;
use std::{hint::black_box, time::Instant};

fn request(delta: Option<bool>, styled: bool, base: Option<&str>) -> Output {
    Output {
        shell_id: "shell".into(),
        lines: Some(200),
        styled: Some(styled),
        if_changed: base.map(str::to_owned),
        wait_ms: None,
        delta,
    }
}

fn wire(result: Result<Value, Fault>) -> Result<Vec<u8>, (&'static str, String)> {
    result
        .map(|value| serde_json::to_vec(&value).unwrap())
        .map_err(|fault| (fault.code, fault.message))
}

#[test]
fn owned_output_matches_legacy_json_errors_and_cache_updates() {
    let old = "\u{1b}[32mrow héllo 🦀 \"quoted\" \\ tab\t\u{1b}[0m\n".repeat(32);
    let changed = format!("{old}one new row");
    let full = json!({
        "output": changed, "hash": "next", "history_size": 300,
        "alternate": false, "rows": 24, "cols": 120,
        "cursor": {"x": 9, "y": 2}, "in_mode": false,
        "unknown": {"output": "not a public field"}
    });
    let mut cases = vec![full.clone(), json!(null), json!([]), json!(17)];
    for output in [
        Value::Null,
        json!(7),
        json!([]),
        json!({}),
        json!(""),
        json!("\n"),
        json!("unsafe\u{1b}[2J"),
        json!("bad\u{0}text"),
        json!("tail\r\n"),
        json!("ΣΟΣ İ ﬁ 🦀"),
    ] {
        let mut value = full.clone();
        value["output"] = output;
        cases.push(value);
    }
    for (key, invalid) in [
        ("hash", json!("has space")),
        ("hash", json!("")),
        ("hash", json!("x".repeat(65))),
        ("hash", json!(7)),
        ("rows", json!(0)),
        ("rows", json!(1000)),
        ("cols", json!(0)),
        ("cursor", json!({"x": 1, "y": 24})),
        ("cursor", json!({"x": -1, "y": 0})),
        ("in_mode", json!(null)),
        ("history_size", json!(-1)),
        ("history_size", json!(u64::MAX)),
        ("alternate", json!("false")),
    ] {
        let mut value = full.clone();
        value[key] = invalid;
        cases.push(value);
    }
    for key in [
        "output",
        "hash",
        "rows",
        "cursor",
        "history_size",
        "alternate",
    ] {
        let mut value = full.clone();
        value.as_object_mut().unwrap().remove(key);
        cases.push(value);
    }
    for hash in [
        json!("base"),
        json!("next"),
        json!("has space"),
        Value::Null,
    ] {
        for output in [json!("\u{1b}[2Jignored in unchanged reply"), json!(null)] {
            cases.push(json!({"unchanged": true, "hash": hash, "output": output,
                "history_size": 301, "alternate": true}));
        }
    }
    for delta in [None, Some(false), Some(true)] {
        for styled in [false, true] {
            for base in [None, Some("base"), Some("missing")] {
                let p = request(delta, styled, base);
                for (index, cli) in cases.iter().enumerate() {
                    let previous = Screens::default();
                    let candidate = Screens::default();
                    let asked = output_delta::Asked {
                        device: "phone",
                        shell: "shell",
                        lines: 200,
                        styled,
                    };
                    for screens in [&previous, &candidate] {
                        screens.remember(&asked, "base", Some(300), old.clone());
                    }
                    assert_eq!(
                        wire(build(&candidate, "phone", &p, 200, styled, cli.clone())),
                        wire(build_legacy(
                            &previous,
                            "phone",
                            &p,
                            200,
                            styled,
                            cli.clone()
                        )),
                        "case {index}, delta {delta:?}, styled {styled}, base {base:?}"
                    );
                    // Probe both possible prior hashes to compare cache mutation as
                    // well as response bytes, including rejected/unchanged replies.
                    for hash in ["base", "next"] {
                        for device in ["phone", "other"] {
                            let follow = request(Some(true), styled, Some(hash));
                            let cli = json!({"output": changed, "hash": "follow",
                                "history_size": 300, "alternate": false});
                            assert_eq!(
                                wire(build(&candidate, device, &follow, 200, styled, cli.clone())),
                                wire(build_legacy(&previous, device, &follow, 200, styled, cli)),
                                "cache after case {index}, base {hash}, device {device}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn owned_output_matches_legacy_across_scrolling_replacement_and_eviction() {
    let previous = Screens::default();
    let candidate = Screens::default();
    let rows: Vec<_> = (0..240)
        .map(|i| format!("row {i}: {}", "héllo 🦀 ".repeat(12)))
        .collect();
    for step in 0..18 {
        let start = step * 3;
        let output = rows[start..start + 120].join("\n");
        let hash = format!("hash-{}", step % 7);
        let base = format!("hash-{}", (step + 6) % 7);
        let p = request(Some(true), false, Some(&base));
        let cli = json!({"hash": hash, "output": output,
            "history_size": 1000 + start, "alternate": false});
        assert_eq!(
            wire(build(&candidate, "phone", &p, 100, false, cli.clone())),
            wire(build_legacy(&previous, "phone", &p, 100, false, cli))
        );
        for lines in [100, 200] {
            for stale in ["hash-0", "hash-2", "hash-6"] {
                let asked = output_delta::Asked {
                    device: "phone",
                    shell: "shell",
                    lines,
                    styled: false,
                };
                assert_eq!(
                    candidate.delta(&asked, stale, Some(1000 + start as u32), &output),
                    previous.delta(&asked, stale, Some(1000 + start as u32), &output)
                );
            }
        }
        if step == 8 {
            previous.forget("phone");
            candidate.forget("phone");
        }
    }
}

#[test]
#[ignore = "manual: paired terminal reply ownership performance measurement"]
fn measure_owned_output_reply() {
    const ITERATIONS: usize = 32;
    for rows in [200, 540, 2000] {
        let old = (0..rows)
            .map(|i| {
                format!(
                    "\u{1b}[32mrow {i}: {}\u{1b}[0m",
                    "terminal héllo 🦀 \"quoted\" \\ ".repeat(8)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        for mode in ["plain", "delta_hit", "delta_miss", "delta_too_large"] {
            let output = if mode == "delta_too_large" {
                old.replace("terminal", "changed!")
            } else {
                format!("{old}\nnew row")
            };
            let delta = (mode != "plain").then_some(true);
            let base = if mode == "delta_miss" {
                "missing"
            } else {
                "base"
            };
            let p = request(delta, true, Some(base));
            let cli = json!({"output": output, "hash": "next", "history_size": 3000,
                "alternate": false, "rows": 24, "cols": 120,
                "cursor": {"x": 4, "y": 2}, "in_mode": false});
            let mut durations = [Vec::new(), Vec::new()];
            for sample in 0..7 {
                for offset in 0..2 {
                    let owned = (sample + offset) % 2;
                    let screens = Screens::default();
                    let asked = output_delta::Asked {
                        device: "phone",
                        shell: "shell",
                        lines: rows,
                        styled: true,
                    };
                    screens.remember(&asked, "base", Some(3000), old.clone());
                    let inputs: Vec<_> = (0..ITERATIONS).map(|_| cli.clone()).collect();
                    let start = Instant::now();
                    for input in inputs {
                        let reply = if owned == 1 {
                            build(&screens, "phone", &p, rows, true, input)
                        } else {
                            build_legacy(&screens, "phone", &p, rows, true, input)
                        };
                        black_box(reply.unwrap());
                    }
                    durations[owned].push(start.elapsed() / ITERATIONS as u32);
                }
            }
            for values in &mut durations {
                values.sort();
            }
            eprintln!(
                "owned_output rows={rows} bytes={} mode={mode} previous={:?} candidate={:?} previous_samples={:?} candidate_samples={:?}",
                output.len(),
                durations[0][3],
                durations[1][3],
                durations[0],
                durations[1]
            );
        }
    }
}

fn build_legacy(
    screens: &Screens,
    device: &str,
    p: &Output,
    lines: u32,
    styled: bool,
    v: Value,
) -> std::result::Result<Value, Fault> {
    let hash = v
        .get("hash")
        .and_then(Value::as_str)
        .filter(|h| hash_shaped(h));
    if v.get("unchanged") == Some(&Value::Bool(true)) {
        // Only ever the answer to a hash this request sent.
        return match (&p.if_changed, hash) {
            (Some(asked), Some(hash)) if asked == hash => {
                let mut result = json!({"shell_id":p.shell_id,"unchanged":true,"hash":hash});
                // Additive: the phone tracks the scrollback's growth.
                if let Some((size, alternate)) = scrollback_fields(&v) {
                    result["history_size"] = json!(size);
                    result["alternate"] = json!(alternate);
                }
                Ok(result)
            }
            _ => Err(cli_fault(
                "CLI reported unchanged output that was not asked",
            )),
        };
    }
    let text = v
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| cli_fault("missing CLI output"))?;
    if styled && !sgr_only(text) {
        return Err(cli_fault(
            "CLI returned escape sequences other than SGR for styled output",
        ));
    }
    let mut result = json!({"shell_id":p.shell_id});
    // Additive: the last `rows` lines of `output` are the visible
    // screen. Absent when the desktop could not read the pane.
    if let (Some(screen), Some(map)) = (screen_fields(&v, text), result.as_object_mut())
        && let Some(fields) = screen.as_object()
    {
        map.extend(fields.clone());
    }
    // Additive: the scrollback lines above the screen, and whether
    // a full-screen program is on the alternate screen.
    if let Some((size, alternate)) = scrollback_fields(&v) {
        result["history_size"] = json!(size);
        result["alternate"] = json!(alternate);
    }
    // Additive: names this exact answer, for `if_changed`.
    if let Some(hash) = hash {
        result["hash"] = json!(hash);
    }
    // Changed rows only, when asked for and this device holds the screen its
    // hash names; the full screen is remembered for the next one either way.
    if p.delta == Some(true)
        && let Some(hash) = hash
    {
        let asked = output_delta::Asked {
            device,
            shell: &p.shell_id,
            lines,
            styled,
        };
        let history = scrollback_fields(&v).map(|(size, _)| size);
        if let Some(base) = &p.if_changed
            && let Some(delta) = screens.delta(&asked, base, history, text)
            && let Some(map) = result.as_object_mut()
        {
            map.insert("delta".into(), delta);
        }
        screens.remember(&asked, hash, history, text.to_owned());
    }
    // A delta already carries all changed text. Only copy the full
    // terminal output into the response when a full reply is needed.
    if result.get("delta").is_none() {
        result["output"] = json!(text);
    }
    Ok(result)
}
