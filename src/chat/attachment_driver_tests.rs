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
            attachments,
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
            attachments,
        })
        .unwrap();
    let events = rx.try_iter().collect::<Vec<_>>();
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
