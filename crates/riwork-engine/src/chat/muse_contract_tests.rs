use super::*;
use crate::chat::model::Provider;
use crate::chat::testkit::{Fake, until};
use std::sync::mpsc;

fn steps(frames: Vec<Value>) -> String {
    frames.into_iter().map(|v| format!("{v}\n")).collect()
}
fn expect(method: &str, reply: Vec<Value>) -> Value {
    json!({"type":"expect","frame":{"method":method},"reply":reply})
}
fn ack(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn refused(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32030,"message":"refused","data":{"kind":"commandRejected"}}})
}
fn handshake() -> Vec<Value> {
    vec![
        expect(
            "initialize",
            vec![ack(
                json!("$id"),
                json!({"grantedCapabilities":["sessionMcp"]}),
            )],
        ),
        expect("initialized", vec![]),
    ]
}
fn fresh() -> Vec<Value> {
    let mut v = handshake();
    v.push(expect("session/start",vec![ack(json!("$id"),json!({"session":{"sessionId":"s","modelId":"working","activeTurnId":null,"turnCount":0,"approvalMode":{"mode":"promptUnmatched"}}}))]));
    v.push(expect(
        "model/list",
        vec![ack(json!("$id"), json!({"models":[]}))],
    ));
    v
}
fn reporter() -> (DriverReporter, mpsc::Receiver<DriverReport>) {
    let (send, recv) = mpsc::channel();
    (
        DriverReporter::new(move |v| {
            let _ = send.send(v);
        }),
        recv,
    )
}
fn configure(model: Option<&str>, effort: Option<&str>, mode: Option<ApprovalMode>) -> ChatCommand {
    ChatCommand::Configure {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        approval_mode: mode,
        fast: None,
        contributor_opt_out: None,
    }
}

#[test]
fn historical_missing_muse_resume_never_sends_a_fresh_start() {
    for historical in [true, false] {
        let mut script = handshake();
        script.push(expect("session/resume",vec![json!({"jsonrpc":"2.0","id":"$id","error":{"code":-32020,"message":"absent","data":{"kind":"sessionNotFound"}}})]));
        if !historical {
            script.extend(fresh().into_iter().skip(2));
        }
        let fake = Fake::new(&[&steps(script)]);
        let mut config = fake.config(Provider::Muse);
        config.resume = Some("saved-session".into());
        config.resume_has_history = historical;
        let (send, recv) = mpsc::channel();
        let result = start(config, send);
        if historical {
            let error = match result {
                Err(error) => error,
                Ok(mut driver) => {
                    driver.shutdown();
                    panic!("a historical session was replaced")
                }
            };
            assert!(error.contains("historical session saved-session"));
            assert!(fake.received_method("session/start").is_empty());
        } else {
            let mut driver = result.unwrap();
            until(&recv, |event| {
                matches!(
                    event,
                    ChatEvent::State {
                        state: ChatState::Idle
                    }
                )
            });
            assert_eq!(driver.provider_thread_id().as_deref(), Some("s"));
            assert_eq!(fake.received_method("session/start").len(), 1);
            driver.shutdown();
        }
        assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
    }
}

#[test]
fn muse_settings_confirm_independent_fields_and_ignore_an_older_model_refusal() {
    let mut script = fresh();
    // Model A remains pending while an independent effort and newer model B settle.
    script.push(expect("session/setModel", vec![])); // id4
    script.push(expect(
        "session/setReasoningEffort",
        vec![ack(json!(5), json!({"status":"accepted"}))],
    ));
    script.push(expect(
        "session/setModel",
        vec![
            ack(json!(6), json!({"status":"accepted"})),
            refused(json!(4)),
        ],
    ));
    script.push(expect("session/setApprovalMode", vec![refused(json!(7))]));
    let fake = Fake::new(&[&steps(script)]);
    let (send, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), send).unwrap();
    until(&events, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Idle
            }
        )
    });
    let (old, old_reports) = reporter();
    driver
        .configure(configure(Some("older"), None, None), old)
        .unwrap();
    let (effort, effort_reports) = reporter();
    driver
        .configure(configure(None, Some("high"), None), effort)
        .unwrap();
    assert_eq!(
        effort_reports
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .effort,
        Some(Some("high".into()))
    );
    let (new, new_reports) = reporter();
    driver
        .configure(configure(Some("newer"), None, None), new)
        .unwrap();
    assert_eq!(
        new_reports
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .model,
        Some(Some("newer".into()))
    );
    let (mode, mode_reports) = reporter();
    driver
        .configure(configure(None, None, Some(ApprovalMode::Full)), mode)
        .unwrap();
    assert_eq!(
        mode_reports
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .approval_mode,
        Some(ApprovalMode::Supervised)
    );
    assert!(
        old_reports.try_recv().is_err(),
        "old refused choice must not roll back the new model"
    );
    let before = fake.received().len();
    assert!(
        driver
            .configure(
                configure(None, Some("invented"), None),
                DriverReporter::default()
            )
            .is_err()
    );
    assert_eq!(
        fake.received().len(),
        before,
        "invalid effort never reaches Muse"
    );
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn muse_provider_observations_override_pending_settings_and_report_actual_mode() {
    let mut script = fresh();
    script.push(expect("session/setModel",vec![
        json!({"jsonrpc":"2.0","method":"session/modelChanged","params":{"sessionId":"s","modelId":"external"}}),
        json!({"jsonrpc":"2.0","method":"session/reasoningEffortChanged","params":{"sessionId":"s","reasoningEffort":"max"}}),
        json!({"jsonrpc":"2.0","method":"session/approvalModeChanged","params":{"sessionId":"s","mode":"onRequest"}}),
        ack(json!(4),json!({"status":"accepted"}))]));
    script.push(expect(
        "session/setApprovalMode",
        vec![ack(
            json!(5),
            json!({"status":"accepted","effectiveMode":{"mode":"promptUnmatched"}}),
        )],
    ));
    let fake = Fake::new(&[&steps(script)]);
    let (mut config, (send, events)) = (fake.config(Provider::Muse), mpsc::channel());
    let (observed, observations) = reporter();
    config.reporter = observed;
    let mut driver = start(config, send).unwrap();
    let initial = observations.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(initial.provider_thread_id.as_deref(), Some("s"));
    assert_eq!(initial.model, Some(Some("working".into())));
    assert_eq!(initial.fast, Some(false));
    until(&events, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Idle
            }
        )
    });
    let (pending, pending_reports) = reporter();
    driver
        .configure(configure(Some("pending"), None, None), pending)
        .unwrap();
    let changes = (0..3)
        .map(|_| observations.recv_timeout(Duration::from_secs(3)).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(changes[0].model, Some(Some("external".into())));
    assert_eq!(changes[1].effort, Some(Some("max".into())));
    assert_eq!(changes[2].approval_mode, Some(ApprovalMode::AutoEdit));
    let (actual, actual_reports) = reporter();
    driver
        .configure(configure(None, None, Some(ApprovalMode::Full)), actual)
        .unwrap();
    assert_eq!(
        actual_reports
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .approval_mode,
        Some(ApprovalMode::Supervised)
    );
    assert!(pending_reports.try_recv().is_err());
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn muses_own_change_notifications_before_replies_confirm_the_request_reporter() {
    let mut script = fresh();
    for (method, notification, value) in [
        (
            "session/setModel",
            "session/modelChanged",
            json!({"modelId":"selected","source":"user"}),
        ),
        (
            "session/setReasoningEffort",
            "session/reasoningEffortChanged",
            json!({"reasoningEffort":"high","source":"user"}),
        ),
        (
            "session/setApprovalMode",
            "session/approvalModeChanged",
            json!({"mode":"onRequest","source":"approvalReconfigure","commandId":"$command_id"}),
        ),
    ] {
        let mut params = value;
        params["sessionId"] = json!("s");
        let mut replies = vec![
            json!({"jsonrpc":"2.0","method":notification,"params":params}),
            ack(
                json!("$id"),
                json!({"status":"accepted","effectiveMode":{"mode":"onRequest"}}),
            ),
        ];
        if method == "session/setApprovalMode" {
            // Muse can fold the original startup mode after session/start's
            // reply. An unchanged observation must not retire this Configure.
            replies.insert(0, json!({"jsonrpc":"2.0","method":"session/approvalModeChanged",
                "params":{"sessionId":"s","mode":"promptUnmatched","source":"approvalReconfigure","commandId":"older-startup"}}));
        }
        script.push(expect(method, replies));
    }
    let fake = Fake::new(&[&steps(script)]);
    let mut config = fake.config(Provider::Muse);
    let (run, run_reports) = reporter();
    config.reporter = run;
    let (send, events) = mpsc::channel();
    let mut driver = start(config, send).unwrap();
    until(&events, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Idle
            }
        )
    });
    run_reports.recv_timeout(Duration::from_secs(3)).unwrap();
    let (request, reports) = reporter();
    driver
        .configure(
            configure(Some("selected"), Some("high"), Some(ApprovalMode::AutoEdit)),
            request,
        )
        .unwrap();
    let mut confirmed = [false; 3];
    while confirmed.iter().any(|confirmed| !confirmed) {
        let report = reports.recv_timeout(Duration::from_secs(3)).unwrap();
        confirmed[0] |= report.model == Some(Some("selected".into()));
        confirmed[1] |= report.effort == Some(Some("high".into()));
        confirmed[2] |= report.approval_mode == Some(ApprovalMode::AutoEdit);
    }
    driver.shutdown();
    let observations: Vec<_> = run_reports.try_iter().collect();
    assert_eq!(
        observations.len(),
        1,
        "only the unchanged startup mode is an unsolicited observation"
    );
    assert_eq!(
        observations[0].approval_mode,
        Some(ApprovalMode::Supervised)
    );
    assert_eq!(observations[0].model, None);
    assert_eq!(observations[0].effort, None);
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn model_discovery_reads_the_provider_without_creating_a_session_or_attaching_cua() {
    let mut script = handshake();
    script.push(expect("model/list", vec![ack(json!("$id"), json!({
        "providerId":"meta", "profileId":null,"source":"providerCatalog",
        "models":[{"modelId":"muse-wonder-1.0","displayLabel":"Wonder","variants":["high","max"]}]
    }))]));
    let fake = Fake::new(&[&steps(script)]);
    let models = discover_models(fake.config(Provider::Muse)).unwrap();
    assert_eq!(models[0].id, "muse-wonder-1.0");
    assert_eq!(models[0].name, "Wonder");
    assert_eq!(models[0].efforts, ["high", "max"]);
    let frames = fake.received();
    assert_eq!(
        frames
            .iter()
            .filter_map(|f| f["method"].as_str())
            .collect::<Vec<_>>(),
        ["initialize", "initialized", "model/list"]
    );
    assert_eq!(
        frames[2]["params"],
        json!({}),
        "no session or command identity"
    );
    assert!(fake.saw("eof"), "the discovery process is reaped");
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn model_discovery_failure_is_returned_and_its_process_is_reaped() {
    let mut script = handshake();
    script.push(expect("model/list", vec![refused(json!("$id"))]));
    let fake = Fake::new(&[&steps(script)]);
    assert!(
        discover_models(fake.config(Provider::Muse))
            .unwrap_err()
            .contains("refused")
    );
    assert!(fake.received_method("session/start").is_empty());
    assert!(fake.saw("eof"));
    assert!(!fake.saw("mismatch"));
}
