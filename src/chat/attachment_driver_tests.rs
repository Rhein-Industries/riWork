//! Exact structured inputs through the real drivers, with fake executable runtimes only.
use super::{
    attachments::*,
    driver::Driver,
    model::*,
    testkit::{Fake, fixture, until},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, io::Cursor, sync::mpsc};

#[test]
fn attachment_start_receipts_require_a_nonempty_id_and_completion_can_arrive_first() {
    for (result, completed_first, unknown) in [
        (json!({}), false, true),
        (json!({"turn":{"id":""}}), false, true),
        (json!({"turn":{"id":"   "}}), false, true),
        (
            json!({"turn":{"id":"finished-before-receipt"}}),
            true,
            false,
        ),
    ] {
        let mut replies = Vec::new();
        if completed_first {
            replies.push(json!({"method":"turn/completed","params":{"threadId":"attachment-thread","turn":{"id":"finished-before-receipt","status":"completed"}}}));
            replies.push(json!({"method":"turn/completed","params":{"threadId":"attachment-thread","turn":{"id":"older-completion-reordered","status":"completed"}}}));
        }
        replies.push(json!({"id":"$id","result":result}));
        let script = fixture("codex/attachments.ndjson")
            .lines()
            .take(4)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
            + &json!({"type":"expect","frame":{"method":"turn/start"},"reply":replies}).to_string();
        let fake = Fake::new(&[&script]);
        let attachments = snapshots(&fake);
        let (mut driver, rx) = start(&fake, Provider::Codex);
        let outcome = driver.command(ChatCommand::SendAttachments {
            text: "draft".into(),
            attachments,
        });
        if unknown {
            assert!(outcome.unwrap_err().starts_with(UNKNOWN_SUBMISSION));
        } else {
            outcome.unwrap();
        }
        assert_eq!(fake.received_method("turn/start").len(), 1);
        assert!(fake.received_method("turn/steer").is_empty());
        assert!(
            !rx.try_iter()
                .any(|event| matches!(event, ChatEvent::TurnStarted { .. })),
            "malformed or completed receipts cannot open a turn"
        );
        driver.shutdown();
    }
}

#[test]
fn codex_attachment_non_draining_child_can_be_cancelled_while_driver_mutex_is_held() {
    use std::{
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };
    let script = fixture("codex/attachments.ndjson")
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n{\"type\":\"attachment_backpressure\"}\n";
    let fake = Fake::new(&[&script]);
    // Text has a tighter admission bound than FILE_BYTES. Keep the original
    // 4 MiB producer load using four individually valid owned snapshots.
    let attachments = (0..4)
        .map(|index| {
            let file = fake.dir.join(format!("large-{index}.txt"));
            fs::write(&file, "x".repeat(TEXT_BYTES)).unwrap();
            let attachment = stage(&fake.dir, &file).unwrap();
            assert_eq!(attachment.bytes, TEXT_BYTES as u64);
            attachment
        })
        .collect::<Vec<_>>();
    assert!(attachments.len() <= SEND_COUNT);
    let total_bytes = attachments.iter().map(|a| a.bytes).sum::<u64>();
    assert_eq!(total_bytes, 4 * TEXT_BYTES as u64);
    assert!(total_bytes <= SEND_BYTES);
    let input_bytes = serde_json::to_vec(&inputs("", &attachments, false).unwrap())
        .unwrap()
        .len();
    let (driver, _) = start(&fake, Provider::Codex);
    let cancel = driver
        .cancel_io()
        .expect("host stop needs cancellation outside the driver lock");
    let driver = Arc::new(Mutex::new(driver));
    let sending = driver.clone();
    let (sent, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        sent.send(
            sending
                .lock()
                .unwrap()
                .command(ChatCommand::SendAttachments {
                    text: String::new(),
                    attachments,
                }),
        )
        .unwrap()
    });
    super::testing::eventually(|| fake.saw("backpressure"));
    let markers = fake
        .entries()
        .into_iter()
        .filter(|entry| entry["backpressure"] == true)
        .collect::<Vec<_>>();
    assert_eq!(markers.len(), 1);
    let prefix_bytes = markers[0]["prefix_bytes"].as_u64().unwrap();
    assert!(prefix_bytes > 0 && prefix_bytes <= 65536);
    assert!(
        driver.try_lock().is_err(),
        "the command must still be in backpressure"
    );
    cancel();
    let error = result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(
        error.starts_with(UNKNOWN_SUBMISSION),
        "partial writes are ambiguous: {error}"
    );
    // Receipt-wait cancellation also returns UNKNOWN_SUBMISSION. Require the
    // writer's byte-counted cancellation, proving the producer was blocked
    // before completing its frame, rather than merely awaiting an RPC reply.
    let written = error
        .split_once("provider input was cancelled (")
        .and_then(|(_, tail)| tail.split_once(" bytes written)"))
        .and_then(|(bytes, _)| bytes.parse::<usize>().ok())
        .expect("cancellation must interrupt the nonblocking producer write");
    assert!(written >= prefix_bytes as usize && written < input_bytes);
    worker.join().unwrap();
    driver.lock().unwrap().shutdown();
    assert!(
        fake.received_method("turn/start").is_empty(),
        "no complete JSON frame was drained"
    );
    assert!(fake.received_method("turn/steer").is_empty());
    assert_eq!(fake.starts().len(), 1, "ambiguous writes must not restart");
    assert_eq!(
        fake.entries()
            .iter()
            .filter(|entry| entry["backpressure"] == true)
            .count(),
        1,
        "only one owned fake enters the prefix-only step"
    );
}

#[test]
fn claude_equal_length_text_versions_keep_distinct_owned_snapshot_history() {
    let script = fixture("claude/attachments.ndjson")
        .lines()
        .take(2)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
        + &json!({"type":"expect","frame":{"type":"user"}}).to_string()
        + "\n"
        + &json!({"type":"expect","frame":{"type":"user"}}).to_string();
    let fake = Fake::new(&[&script]);
    let file = fake.dir.join("same.txt");
    fs::write(&file, "version A").unwrap();
    let first = stage(&fake.dir, &file).unwrap();
    fs::write(&file, "version B").unwrap();
    let second = stage(&fake.dir, &file).unwrap();
    assert_eq!(first.bytes, second.bytes);
    let (mut driver, rx) = start(&fake, Provider::Claude);
    for snapshot in [&first, &second] {
        driver
            .command(ChatCommand::SendAttachments {
                text: String::new(),
                attachments: vec![snapshot.clone()],
            })
            .unwrap();
    }
    let history = rx
        .try_iter()
        .filter_map(|event| match event {
            ChatEvent::ItemCompleted { item }
                if matches!(item.body, ItemBody::UserMessage { .. }) =>
            {
                Some(item)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].body, history[1].body,
        "display summaries alone are insufficient"
    );
    let durable: Vec<Item> =
        serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
    assert_eq!(durable[0].presentation.attachments, vec![first.clone()]);
    assert_eq!(durable[1].presentation.attachments, vec![second.clone()]);
    assert_ne!(first.id, second.id);
    assert_ne!(first.fingerprint, second.fingerprint);
    assert_eq!(
        fs::read(&durable[0].presentation.attachments[0].path).unwrap(),
        b"version A"
    );
    assert_eq!(
        fs::read(&durable[1].presentation.attachments[0].path).unwrap(),
        b"version B"
    );
    driver.shutdown();
}

#[test]
fn large_accepted_png_codex_user_echo_keeps_exact_bytes_in_durable_history() {
    // Deterministic incompressible pixels produce a valid PNG above the former
    // 3 MiB raw / 4 MiB encoded boundary, without exceeding the staging cap.
    let mut seed = 0x7a12_098bu32;
    let pixels = (0..1024 * 1100 * 3)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect::<Vec<_>>();
    let image = image::RgbImage::from_raw(1024, 1100, pixels).unwrap();
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    assert!(png.len() > 3 << 20 && png.len() <= FILE_BYTES as usize);
    let payload = STANDARD.encode(&png);
    let script = fixture("codex/attachments.ndjson").lines().take(4).collect::<Vec<_>>().join("\n") + "\n" +
        &json!({"type":"expect","frame":{"method":"turn/start"},"reply":[
            {"id":"$id","result":{"turn":{"id":"image-turn"}}},
            {"method":"item/completed","params":{"threadId":"attachment-thread","turnId":"image-turn","item":{"id":"image-user","type":"userMessage","content":[{"type":"image","url":format!("data:image/png;base64,{payload}")}]}}}
        ]}).to_string();
    let fake = Fake::new(&[&script]);
    let file = fake.dir.join("large.png");
    fs::write(&file, &png).unwrap();
    let attachment = stage(&fake.dir, &file).unwrap();
    let (mut driver, rx) = start(&fake, Provider::Codex);
    driver
        .command(ChatCommand::SendAttachments {
            text: String::new(),
            attachments: vec![attachment],
        })
        .unwrap();
    let events = until(
        &rx,
        |event| matches!(event, ChatEvent::ItemCompleted { item } if item.id == "image-user"),
    );
    let item = events
        .into_iter()
        .find_map(|event| match event {
            ChatEvent::ItemCompleted { item } if item.id == "image-user" => Some(item),
            _ => None,
        })
        .unwrap();
    let durable: Item = serde_json::from_slice(&serde_json::to_vec(&item).unwrap()).unwrap();
    let ImageSource::Data { base64, mime } = &durable.presentation.images[0].source else {
        panic!("large accepted PNG was not retained");
    };
    assert_eq!(mime, "image/png");
    assert_eq!(STANDARD.decode(base64).unwrap(), png);
    driver.shutdown();
}

fn snapshots(fake: &Fake) -> Vec<Attachment> {
    let file = fake.dir.join("readme.md");
    fs::write(&file, "actual file bytes 🦀\n").unwrap();
    let text = stage(&fake.dir, &file).unwrap();
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(2, 1)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    let file = fake.dir.join("photo.png");
    fs::write(&file, bytes).unwrap();
    let png = stage(&fake.dir, &file).unwrap();
    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(3, 2)
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();
    let file = fake.dir.join("photo.jpg");
    fs::write(&file, jpeg).unwrap();
    vec![text, png, stage(&fake.dir, &file).unwrap()]
}
// Construct expectations independently of the production input encoder.
fn expected(attachments: &[Attachment], claude: bool) -> Value {
    let mut blocks =
        vec![json!({"type":"text","text":"Attached file: readme.md\nactual file bytes 🦀\n"})];
    for (attachment, mime) in attachments[1..].iter().zip(["image/png", "image/jpeg"]) {
        let bytes = fs::read(&attachment.path).unwrap();
        blocks.push(json!({"type":"text","text":format!("Attached image: {}",attachment.name)}));
        blocks.push(if claude {json!({"type":"image","source":{"type":"base64","media_type":mime,"data":STANDARD.encode(bytes)}})}
            else {json!({"type":"image","url":format!("data:{mime};base64,{}",STANDARD.encode(bytes))})});
    }
    Value::Array(blocks)
}
fn start(fake: &Fake, provider: Provider) -> (Box<dyn Driver>, mpsc::Receiver<ChatEvent>) {
    let (tx, rx) = mpsc::channel();
    let driver = match provider {
        Provider::Codex => super::codex::start(fake.config(provider), tx),
        Provider::Claude => super::claude::start(fake.config(provider), tx),
    }
    .unwrap();
    until(&rx, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Idle
            }
        )
    });
    (driver, rx)
}
#[test]
fn codex_attachments_send_exact_bytes_and_refused_steer_never_retries_as_start() {
    let fake = Fake::new(&[&fixture("codex/attachments.ndjson")]);
    let attachments = snapshots(&fake);
    let expected = expected(&attachments, false);
    // Selection owns its bytes even after client source files disappear.
    for name in ["readme.md", "photo.png", "photo.jpg"] {
        fs::remove_file(fake.dir.join(name)).unwrap();
    }
    let (mut driver, rx) = start(&fake, Provider::Codex);
    driver
        .command(ChatCommand::SendAttachments {
            text: String::new(),
            attachments: attachments.clone(),
        })
        .unwrap();
    assert!(
        !rx.try_iter()
            .any(|e| matches!(e, ChatEvent::TurnCompleted { .. })),
        "submission is not completion"
    );
    assert_eq!(
        fake.received_method("turn/start")[0]["params"]["input"],
        expected
    );
    let err = driver
        .command(ChatCommand::SendAttachments {
            text: "steer".into(),
            attachments: attachments.clone(),
        })
        .unwrap_err();
    assert_eq!(err, "attachment steer refused");
    assert_eq!(fake.received_method("turn/start").len(), 1);
    assert_eq!(fake.received_method("turn/steer").len(), 1);
    driver
        .command(ChatCommand::Send {
            text: "legacy".into(),
        })
        .unwrap();
    driver.shutdown();
    assert!(!fake.saw("mismatch"));
}
#[test]
fn codex_provider_refusal_and_lost_reply_are_distinct_and_neither_retries() {
    for (fixture_name, unknown) in [
        ("codex/attachments_refused.ndjson", false),
        ("codex/attachments_lost.ndjson", true),
    ] {
        let fake = Fake::new(&[&fixture(fixture_name)]);
        let attachments = snapshots(&fake);
        let (mut driver, _) = start(&fake, Provider::Codex);
        let err = driver
            .command(ChatCommand::SendAttachments {
                text: "draft".into(),
                attachments,
            })
            .unwrap_err();
        assert_eq!(err.starts_with(UNKNOWN_SUBMISSION), unknown, "{err}");
        assert_eq!(fake.received_method("turn/start").len(), 1);
        driver.shutdown();
        assert!(!fake.saw("mismatch"));
    }
}
#[test]
fn claude_attachment_only_send_is_content_array_with_actual_bytes_and_local_preview_echo() {
    let fake = Fake::new(&[&fixture("claude/attachments.ndjson")]);
    let attachments = snapshots(&fake);
    let expected = expected(&attachments, true);
    let (mut driver, rx) = start(&fake, Provider::Claude);
    driver
        .command(ChatCommand::SendAttachments {
            text: String::new(),
            attachments: attachments.clone(),
        })
        .unwrap();
    let events = rx.try_iter().collect::<Vec<_>>();
    let echo = events
        .iter()
        .find_map(|event| match event {
            ChatEvent::ItemCompleted { item }
                if matches!(item.body, ItemBody::UserMessage { .. }) =>
            {
                Some(item)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(echo.presentation.attachments, attachments);
    let durable: Item = serde_json::from_slice(&serde_json::to_vec(echo).unwrap()).unwrap();
    assert_eq!(durable.presentation.attachments, attachments);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::TurnCompleted { .. }))
    );
    assert!(events.iter().any(|e|matches!(e,ChatEvent::ItemStarted {item} if item.presentation.images.len()==2 && matches!(&item.body,ItemBody::UserMessage {text} if text.contains("readme.md") && text.contains("photo.png") && text.contains("photo.jpg")))));
    // Writer receipt is local submission; fake may still be reading the flushed pipe.
    super::testing::eventually(|| fake.received().iter().any(|f| f["type"] == "user"));
    assert_eq!(
        fake.received()
            .iter()
            .find(|f| f["type"] == "user")
            .unwrap()["message"]["content"],
        expected
    );
    driver
        .command(ChatCommand::Send {
            text: "legacy".into(),
        })
        .unwrap();
    until(&rx, |e| {
        matches!(
            e,
            ChatEvent::TurnCompleted {
                outcome: TurnOutcome::Completed,
                ..
            }
        )
    });
    driver.shutdown();
    assert!(!fake.saw("mismatch"));
}
#[test]
fn drivers_refuse_invalid_set_without_writing_even_plain_text() {
    for provider in [Provider::Codex, Provider::Claude] {
        let name = if provider == Provider::Codex {
            "codex/attachments.ndjson"
        } else {
            "claude/attachments.ndjson"
        };
        let fake = Fake::new(&[&fixture(name)]);
        let mut attachments = snapshots(&fake);
        let (mut driver, _) = start(&fake, provider);
        fs::remove_file(&attachments[1].path).unwrap();
        assert!(
            driver
                .command(ChatCommand::SendAttachments {
                    text: "never sent".into(),
                    attachments: attachments.clone()
                })
                .unwrap_err()
                .contains("photo.png")
        );
        assert!(fake.received_method("turn/start").is_empty());
        assert!(!fake.received().iter().any(|f| f["type"] == "user"));
        attachments[0].bytes = FILE_BYTES + 1;
        assert!(
            driver
                .command(ChatCommand::SendAttachments {
                    text: "never sent".into(),
                    attachments
                })
                .is_err()
        );
        driver.shutdown();
        // EOF at an unmet expectation is intentional in this refusal fixture.
    }
}

#[test]
fn codex_attachment_steer_checks_receipt_and_exact_bytes_without_restart() {
    for turn_id in ["attachment-turn", "unexpected-turn"] {
        let mut script = fixture("codex/attachments.ndjson");
        script = script.lines().take(5).collect::<Vec<_>>().join("\n");
        script.push_str(&format!("\n{}\n",json!({"type":"expect","frame":{"method":"turn/steer"},"reply":[{"id":"$id","result":{"turnId":turn_id}}]})));
        let fake = Fake::new(&[&script]);
        let attachments = snapshots(&fake);
        let (mut driver, _) = start(&fake, Provider::Codex);
        driver
            .command(ChatCommand::SendAttachments {
                text: String::new(),
                attachments: attachments.clone(),
            })
            .unwrap();
        let result = driver.command(ChatCommand::SendAttachments {
            text: String::new(),
            attachments: attachments.clone(),
        });
        if turn_id == "attachment-turn" {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().starts_with(UNKNOWN_SUBMISSION));
        }
        assert_eq!(
            fake.received_method("turn/steer")[0]["params"]["input"],
            expected(&attachments, false)
        );
        assert_eq!(fake.received_method("turn/start").len(), 1);
        driver.shutdown();
        assert!(!fake.saw("mismatch"));
    }
}

#[test]
fn codex_attachments_refuse_unknown_starting_turn_and_claude_refuses_stopped_writer() {
    let script = fixture("codex/attachments.ndjson").lines().take(4).collect::<Vec<_>>().join("\n") + "\n" +
        &json!({"type":"expect","frame":{"method":"turn/start","params":{"input":[{"type":"text","text":"legacy starting"}]}},"delay_ms":300,"reply":[{"id":"$id","result":{"turn":{"id":"attachment-turn"}}}]}).to_string();
    let fake = Fake::new(&[&script]);
    let attachments = snapshots(&fake);
    let (mut driver, _) = start(&fake, Provider::Codex);
    driver
        .command(ChatCommand::Send {
            text: "legacy starting".into(),
        })
        .unwrap();
    assert!(
        driver
            .command(ChatCommand::SendAttachments {
                text: "keep me".into(),
                attachments
            })
            .unwrap_err()
            .contains("starting")
    );
    super::testing::eventually(|| fake.received_method("turn/start").len() == 1);
    assert!(fake.received_method("turn/steer").is_empty());
    driver.shutdown();
    let script = fixture("claude/attachments.ndjson")
        .lines()
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    let fake = Fake::new(&[&script]);
    let attachments = snapshots(&fake);
    let (mut driver, _) = start(&fake, Provider::Claude);
    driver.shutdown();
    assert!(
        driver
            .command(ChatCommand::SendAttachments {
                text: "keep me".into(),
                attachments
            })
            .unwrap_err()
            .contains("not ready")
    );
    assert!(!fake.received().iter().any(|f| f["type"] == "user"));
}
