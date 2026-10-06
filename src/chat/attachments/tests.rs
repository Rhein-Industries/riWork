use super::*;
use crate::chat::{
    model::{ChatCommand, Provider},
    testing::TestHost,
};

fn source(host: &TestHost, name: &str, bytes: &[u8]) -> PathBuf {
    let path = host.work().join(name);
    fs::write(&path, bytes).unwrap();
    path
}
#[test]
fn owned_snapshots_survive_source_edits_and_have_private_bounded_previews() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let source = source(&host, "notes.md", "hello 🦀\n".repeat(150).as_bytes());
    let a = host.client().stage_attachment(&chat.id, &source).unwrap();
    fs::write(&source, b"changed").unwrap();
    fs::remove_file(source).unwrap();
    assert_eq!(load(&a).unwrap(), "hello 🦀\n".repeat(150).as_bytes());
    let Preview::Text { excerpt } = &a.preview else {
        panic!()
    };
    assert_eq!(excerpt.chars().count(), TEXT_PREVIEW_CHARS);
    assert_eq!(
        fs::metadata(&a.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(a.path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        inputs("", &[a.clone()], false).unwrap()[0]["text"],
        format!("Attached file: notes.md\n{}", "hello 🦀\n".repeat(150))
    );
    let second = host.create_in("other", Provider::Codex);
    assert!(
        host.client()
            .command_checked(
                &second.id,
                ChatCommand::SendAttachments {
                    text: "do not lose".into(),
                    attachments: vec![a.clone()]
                }
            )
            .unwrap_err()
            .to_string()
            .contains("another chat")
    );
    assert!(
        crate::chat::testing::fake_for(&second.cwd)
            .commands()
            .is_empty()
    );
    fs::write(&a.path, b"corruption").unwrap();
    let err = host
        .client()
        .command_checked(
            &chat.id,
            ChatCommand::SendAttachments {
                text: "do not lose".into(),
                attachments: vec![a],
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("snapshot changed"));
    assert!(host.fake().commands().is_empty());
}
#[test]
fn unsupported_missing_symlink_and_limits_refuse_before_provider_submission() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    for (name, bytes) in [
        ("binary", b"\0x".as_slice()),
        ("fake.png", b"bad"),
        ("manual.pdf", b"%PDF-1.7"),
    ] {
        let err = host
            .client()
            .stage_attachment(&chat.id, &source(&host, name, bytes))
            .unwrap_err();
        assert!(err.to_string().contains(name));
    }
    assert!(
        host.client()
            .stage_attachment(&chat.id, &host.work())
            .is_err()
    );
    assert!(
        host.client()
            .stage_attachment(&chat.id, &host.work().join("gone"))
            .is_err()
    );
    let file = source(&host, "good.txt", b"contents");
    let link = host.work().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(host.client().stage_attachment(&chat.id, &link).is_err());
    let huge = source(&host, "huge", &vec![b'x'; TEXT_BYTES + 1]);
    assert!(host.client().stage_attachment(&chat.id, &huge).is_err());
    let a = host.client().stage_attachment(&chat.id, &file).unwrap();
    for attachments in [vec![], vec![a.clone(); 2], vec![a.clone(); SEND_COUNT + 1]] {
        assert!(
            host.client()
                .command_checked(
                    &chat.id,
                    ChatCommand::SendAttachments {
                        text: "keep me".into(),
                        attachments
                    }
                )
                .is_err()
        );
    }
    fs::remove_file(&a.path).unwrap();
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: "keep me".into(),
                    attachments: vec![a]
                }
            )
            .is_err()
    );
    assert!(host.fake().commands().is_empty());
}
#[test]
fn actual_image_bytes_and_thumbnail_match_both_provider_formats() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(512, 384)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    let a = host
        .client()
        .stage_attachment(&chat.id, &source(&host, "photo.png", &bytes))
        .unwrap();
    let Preview::Image { path } = &a.preview else {
        panic!()
    };
    let thumbnail = image::open(path).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (256, 192));
    let claude = inputs("inspect", &[a.clone()], true).unwrap();
    assert_eq!(claude[2]["source"]["media_type"], "image/png");
    assert_eq!(
        STANDARD
            .decode(claude[2]["source"]["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    let codex = inputs("inspect", &[a], false).unwrap();
    assert_eq!(
        codex[2]["url"],
        format!("data:image/png;base64,{}", STANDARD.encode(bytes))
    );
}
#[test]
fn retention_count_quota_survives_host_restart_and_staging_does_not_resume_provider() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let file = source(&host, "file.txt", b"x");
    host.client().close(&chat.id).unwrap();
    let starts = host.fake().start_count();
    for _ in 0..STORE_COUNT {
        host.client().stage_attachment(&chat.id, &file).unwrap();
    }
    host.restart(crate::chat::testing::quick_options());
    assert!(
        host.client()
            .stage_attachment(&chat.id, &file)
            .unwrap_err()
            .to_string()
            .contains("128 file limit")
    );
    assert_eq!(host.fake().start_count(), starts);
    host.client().delete(&chat.id).unwrap();
    assert!(!host.home.join("chats").join(chat.id).exists());
}

#[test]
fn old_host_refuses_attachment_operations_without_receiving_legacy_text_fallback() {
    use crate::chat::{
        client::{CallError, Client},
        wire::{Request, Response},
    };
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
        time::Duration,
    };
    #[derive(serde::Deserialize)]
    #[serde(tag = "command", rename_all = "snake_case")]
    enum OldCommand {
        Send { text: String },
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum OldRequest {
        Capabilities { id: String },
        Command { id: String, command: OldCommand },
    }
    let home = crate::chat::testing::short_home();
    let socket = home.join("old.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let serving = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut sends = Vec::new();
        let mut ops = Vec::new();
        for _ in 0..4 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            ops.push(value["op"].clone());
            let (id, ok, result, error) = match serde_json::from_str::<OldRequest>(&line) {
                Ok(OldRequest::Capabilities { id }) => {
                    (id, true, Some(json!({"identified_create":true})), None)
                }
                Ok(OldRequest::Command {
                    id,
                    command: OldCommand::Send { text },
                }) => {
                    sends.push(text);
                    (id, true, None, None)
                }
                Err(e) => (
                    value["id"].as_str().unwrap().into(),
                    false,
                    None,
                    Some(e.to_string()),
                ),
            };
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&Response {
                    id,
                    ok,
                    result,
                    error
                })
                .unwrap()
            )
            .unwrap();
        }
        (ops, sends)
    });
    let mut client = Client::connect(&socket).unwrap();
    assert!(
        client
            .supports_identified_create(Duration::from_secs(2))
            .unwrap()
    );
    assert!(matches!(
        client.stage_attachment("chat", &home.join("no-read")),
        Err(CallError::Refused(_))
    ));
    assert!(matches!(
        client.command_checked(
            "chat",
            ChatCommand::SendAttachments {
                text: "must not degrade".into(),
                attachments: Vec::new()
            }
        ),
        Err(CallError::Refused(_))
    ));
    client
        .command(
            "chat",
            ChatCommand::Send {
                text: "legacy still works".into(),
            },
        )
        .unwrap();
    let (ops, sends) = serving.join().unwrap();
    assert_eq!(sends, vec!["legacy still works"]);
    assert_eq!(
        ops,
        vec![
            json!("capabilities"),
            json!("stage_attachment"),
            json!("command"),
            json!("command")
        ]
    );
    assert_eq!(
        serde_json::to_value(ChatCommand::Send {
            text: "hello".into()
        })
        .unwrap(),
        json!({"command":"send","text":"hello"})
    );
    assert_eq!(
        serde_json::to_value(crate::chat::wire::Capabilities {
            identified_create: true
        })
        .unwrap(),
        json!({"identified_create":true})
    );
    // New unknown-submission errors keep the client's existing Refused/Broken distinction.
    assert!(
        serde_json::from_value::<Request>(
            json!({"op":"stage_attachment","id":"x","chat_id":"chat","path":"/file"})
        )
        .is_ok()
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn valid_attachment_submission_resumes_stopped_chat_and_ack_is_not_turn_completion() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let a = host
        .client()
        .stage_attachment(&chat.id, &source(&host, "file.txt", b"actual content"))
        .unwrap();
    host.client().close(&chat.id).unwrap();
    let starts = host.fake().start_count();
    host.client()
        .command_checked(
            &chat.id,
            ChatCommand::SendAttachments {
                text: "hang".into(),
                attachments: vec![a.clone()],
            },
        )
        .unwrap();
    assert_eq!(host.fake().start_count(), starts + 1);
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, crate::chat::model::ChatEvent::TurnStarted { .. }))
    });
    // A receipt returns while the fake turn hangs, and exact descriptors reached the driver.
    assert!(!matches!(
        log.last().unwrap().event,
        crate::chat::model::ChatEvent::TurnCompleted { .. }
    ));
    assert!(
        matches!(host.fake().commands().last().unwrap(),ChatCommand::SendAttachments {attachments,..} if attachments == &[a])
    );
}

#[test]
fn attachment_metadata_and_storage_links_are_refused_before_resuming() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let a = host
        .client()
        .stage_attachment(&chat.id, &source(&host, "file.txt", b"content"))
        .unwrap();
    host.client().close(&chat.id).unwrap();
    let starts = host.fake().start_count();
    let mut altered = a.clone();
    altered.name = "different.txt".into();
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: "draft".into(),
                    attachments: vec![altered]
                }
            )
            .unwrap_err()
            .to_string()
            .contains("metadata changed")
    );
    let dir = a.path.parent().unwrap();
    let moved = dir.with_extension("saved");
    fs::rename(dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, dir).unwrap();
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: "draft".into(),
                    attachments: vec![a.clone()]
                }
            )
            .unwrap_err()
            .to_string()
            .contains("unsafe snapshot")
    );
    fs::remove_file(dir).unwrap();
    fs::rename(moved, dir).unwrap();
    fs::remove_file(&a.path).unwrap();
    std::os::unix::fs::symlink(host.work().join("file.txt"), &a.path).unwrap();
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: "draft".into(),
                    attachments: vec![a]
                }
            )
            .is_err()
    );
    assert_eq!(host.fake().start_count(), starts);
    assert!(host.fake().commands().is_empty());
}

#[test]
fn attachment_encoded_bounds_and_preview_storage_bytes_are_counted_before_start() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let file = source(&host, "escaped.txt", &vec![b'\r'; TEXT_BYTES]);
    let attachments = (0..SEND_COUNT)
        .map(|_| host.client().stage_attachment(&chat.id, &file).unwrap())
        .collect::<Vec<_>>();
    host.client().close(&chat.id).unwrap();
    let starts = host.fake().start_count();
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: String::new(),
                    attachments: attachments.clone()
                }
            )
            .unwrap_err()
            .to_string()
            .contains("encoded attachment input")
    );
    let mut oversized = attachments[..3].to_vec();
    for a in &mut oversized {
        a.bytes = FILE_BYTES;
    }
    assert!(
        host.client()
            .command_checked(
                &chat.id,
                ChatCommand::SendAttachments {
                    text: "draft".into(),
                    attachments: oversized
                }
            )
            .unwrap_err()
            .to_string()
            .contains("8 MiB send limit")
    );
    // A sparse retained preview verifies byte quota without allocating 64 MiB.
    let preview = attachments[0]
        .path
        .parent()
        .unwrap()
        .join("retained-preview.png");
    fs::File::create(preview)
        .unwrap()
        .set_len(STORE_BYTES)
        .unwrap();
    assert!(
        host.client()
            .stage_attachment(&chat.id, &file)
            .unwrap_err()
            .to_string()
            .contains("storage exceeds 64 MiB")
    );
    assert_eq!(host.fake().start_count(), starts);
    assert!(host.fake().commands().is_empty());
}
