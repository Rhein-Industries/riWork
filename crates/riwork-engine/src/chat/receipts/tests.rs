use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        Self(crate::chat::testing::short_home())
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn message() -> ChatCommand {
    ChatCommand::Send {
        text: "private prompt that need not be stored again".into(),
    }
}

#[test]
fn command_receipts_reserve_once_across_concurrent_connections() {
    let home = Home::new();
    let barrier = Arc::new(Barrier::new(8));
    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let (path, barrier) = (home.0.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                reserve(&path, "chat-a", "one-operation", &message(), false).unwrap()
            })
        })
        .collect();
    assert_eq!(
        attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap())
            .filter(|reservation| matches!(reservation, Reservation::New(_)))
            .count(),
        1
    );
    let path = receipt_path(&home.0, "one-operation");
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!fs::read_to_string(path).unwrap().contains("private prompt"));
    assert_eq!(
        read(&home.0, "chat-a", "one-operation")
            .unwrap()
            .unwrap()
            .status,
        CommandStatus::Uncertain
    );
}

#[test]
fn command_receipts_bind_a_global_key_to_chat_and_canonical_command() {
    let home = Home::new();
    let first: ChatCommand =
        serde_json::from_str(r#"{"command":"configure","fast":true,"model":"model-a"}"#).unwrap();
    let reordered: ChatCommand = serde_json::from_str(
        r#"{"model":"model-a","fast":true,"effort":null,"command":"configure"}"#,
    )
    .unwrap();
    assert!(matches!(
        reserve(&home.0, "chat-a", "key", &first, false),
        Ok(Reservation::New(_))
    ));
    assert!(matches!(
        reserve(&home.0, "chat-a", "key", &reordered, false),
        Ok(Reservation::Existing(_))
    ));
    assert!(
        matches!(reserve(&home.0, "chat-b", "key", &first, false), Err(error) if error.starts_with(OPERATION_CONFLICT))
    );
    assert!(
        matches!(reserve(&home.0, "chat-a", "key", &message(), false), Err(error) if error.starts_with(OPERATION_CONFLICT))
    );
}

#[test]
fn command_receipts_fail_closed_on_corruption_or_invalid_operation_ids() {
    let home = Home::new();
    for invalid in ["", "a\nb", "a\0b"] {
        assert!(reserve(&home.0, "chat-a", invalid, &message(), false).is_err());
    }
    assert!(reserve(&home.0, "chat-a", &"a".repeat(257), &message(), false).is_err());
    reserve(&home.0, "chat-a", "key", &message(), false).unwrap();
    fs::write(receipt_path(&home.0, "key"), b"{truncated").unwrap();
    assert!(read(&home.0, "chat-a", "key").is_err());
    assert!(reserve(&home.0, "chat-a", "key", &message(), false).is_err());
}
