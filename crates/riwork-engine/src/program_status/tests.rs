use super::*;

struct Fixture {
    home: PathBuf,
    shell: String,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("somtus-status-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        Self {
            home,
            shell: Uuid::new_v4().to_string(),
        }
    }
    fn start(&self) -> Reports {
        Reports::new(&self.shell, &prepare(&self.home, &self.shell).unwrap())
    }
    fn session(&self, harness: Option<&str>) -> crate::sessions::ShellSession {
        let mut shell: crate::sessions::ShellSession = serde_json::from_value(serde_json::json!({
            "id":self.shell,"project_id":null,"worktree_id":null,"kind":"project",
            "cwd":self.home,"command":null,"created_at_unix":1,"harness":harness
        }))
        .unwrap();
        shell.alive = true;
        shell
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn commands(bytes: &[u8], chunk: usize) -> Vec<Command> {
    let mut parser = Parser::default();
    let mut out = Vec::new();
    for part in bytes.chunks(chunk) {
        parser.feed(part, |c| out.push(c));
    }
    out
}
fn report(body: &str) -> Command {
    parse(format!("7501;{body}").as_bytes(), false).unwrap()
}

#[test]
fn chunked_bel_st_queries_and_unicode_fields_are_exact() {
    let sequence = b"ordinary text\x1b]0;title\x07\x1b]7501;?\x07\x1b]7501;state=blocked:kind=permission:progress=40:app=agent:msg=QXBwbHk/:title=8J+QkQ==\x1b\\\x1b]7501;state=clear:id=task/one\x07";
    let expected = commands(sequence, sequence.len());
    assert_eq!(expected.len(), 3);
    assert_eq!(expected[0], Command::Query { bell: true });
    let Command::Report { id, record } = &expected[1] else {
        panic!("report")
    };
    assert!(id.is_empty());
    assert_eq!(record.state, State::Blocked);
    assert_eq!(record.kind, Some(Kind::Permission));
    assert_eq!(record.progress, Some(40));
    assert_eq!(record.message.as_deref(), Some("Apply?"));
    assert_eq!(record.title.as_deref(), Some("🐑"));
    for chunk in 1..=sequence.len() {
        assert_eq!(commands(sequence, chunk), expected, "chunk {chunk}");
    }
}

#[test]
fn invalid_sequences_are_dropped_without_hiding_the_next_report() {
    for body in [
        "state=unknown",
        "app=agent",
        "state=clear:id=bad//id",
        "state=working:id=",
        "state=done:msg=%%%",
        "state=done:msg=AA==",
        "state=working:title=/w==",
    ] {
        // Characters outside the protocol alphabet are ignored fields; invalid
        // base64 inside the alphabet, control text and invalid ids are rejected.
        if body.contains("%%") {
            continue;
        }
        assert!(
            parse(format!("7501;{body}").as_bytes(), false).is_none(),
            "{body}"
        );
    }
    let oversized = format!(
        "\x1b]7501;state=working:msg={}\x1b\\\x1b]7501;state=done\x07",
        "A".repeat(100_000)
    );
    let mut parser = Parser::default();
    let mut out = Vec::new();
    for bytes in oversized.as_bytes().chunks(31) {
        parser.feed(bytes, |c| out.push(c));
        assert!(parser.body.len() <= MAX_BODY + 5);
    }
    assert_eq!(out, vec![report("state=done")]);
    for body in [
        format!("state=working:app={}:app=x", "a".repeat(33)),
        format!("state=idle:title={}", STANDARD.encode("a".repeat(193))),
        format!("state=idle:msg={}", STANDARD.encode("a".repeat(2049))),
        format!("state=idle:id={}", "a/".repeat(8) + "b"),
    ] {
        assert!(parse(format!("7501;{body}").as_bytes(), false).is_none());
    }
}

#[test]
fn terminal_strings_cancellation_and_optional_fields_do_not_infer_status() {
    let bytes = b"working done blocked\x1bP\x1b]7501;state=done\x07\x1b\\\x1b]7501;state=working\x18\x1b]7501;state=idle:kind=auth:progress=99\x07";
    assert_eq!(commands(bytes, 1), vec![report("state=idle")]);
    let Command::Report { record, .. } =
        report("state=working:progress=999:kind=auth:app=bad/name")
    else {
        panic!()
    };
    assert_eq!(
        (record.kind, record.progress, record.app),
        (None, None, None)
    );
    assert_eq!(report("state=working:state=done"), report("state=done"));
}

#[test]
fn complete_record_replacement_clear_hierarchy_and_lru_are_bounded() {
    let f = Fixture::new();
    let mut reports = f.start();
    reports.apply(report("state=working:progress=10:app=agent"), 10);
    reports.apply(report("state=working"), 20);
    let root = reports.value.root.as_ref().unwrap();
    assert_eq!(root.since_unix, 10);
    assert_eq!(root.progress, None);
    assert_eq!(root.app, None);
    for id in ["task", "task/one", "taskx"] {
        reports.apply(report(&format!("state=working:id={id}")), 21);
    }
    reports.apply(report("state=clear:id=task"), 22);
    assert_eq!(
        reports.value.records.keys().cloned().collect::<Vec<_>>(),
        ["taskx"]
    );
    for i in 0..300 {
        reports.apply(report(&format!("state=working:id=r{i}")), 23);
    }
    assert_eq!(reports.value.records.len(), 255);
    assert!(!reports.value.records.contains_key("r0"));
    reports.apply(report("state=working:id=r45"), 24);
    reports.apply(report("state=working:id=new"), 25);
    assert!(reports.value.records.contains_key("r45"));
    assert!(!reports.value.records.contains_key("r46"));
    reports.apply(report("state=clear"), 26);
    assert!(reports.value.root.is_none() && reports.value.records.is_empty());
}

#[test]
fn collector_eof_and_expired_lease_are_not_completion() {
    let f = Fixture::new();
    let token = prepare(&f.home, &f.shell).unwrap();
    let mut replies = Vec::new();
    collect(
        &f.home,
        &f.shell,
        &token,
        &b"\x1b]7501;?\x07\x1b]7501;state=working\x1b\\"[..],
        &mut replies,
    )
    .unwrap();
    assert_eq!(replies, format!("\x1b]7501;{CAPABILITIES}\x07").as_bytes());
    let mut saved = snapshot(&f.home, &f.shell).unwrap();
    assert!(!saved.connected);
    assert_eq!(saved.root.as_ref().unwrap().state, State::Working);
    saved.connected = true;
    saved.updated_at_unix = crate::activity::unix_now() - LEASE_SECONDS - 1;
    save(&f.home, &saved).unwrap();
    assert!(!snapshot(&f.home, &f.shell).unwrap().connected);
}

#[test]
fn old_collector_cannot_publish_into_new_incarnation() {
    let f = Fixture::new();
    let mut old = f.start();
    old.apply(report("state=done"), 10);
    save(&f.home, &old.value).unwrap();
    let token = prepare(&f.home, &f.shell).unwrap();
    old.apply(report("state=working"), 20);
    save(&f.home, &old.value).unwrap();
    let current = snapshot(&f.home, &f.shell).unwrap();
    assert_eq!(current.generation, token);
    assert!(current.root.is_none());
    assert!(
        collect(
            &f.home,
            &f.shell,
            &old.value.generation,
            &b""[..],
            Vec::new()
        )
        .is_err()
    );
}

#[test]
fn retained_pipe_switches_generation_only_at_launcher_boundary() {
    struct Restart<'a> {
        fixture: &'a Fixture,
        phase: u8,
    }
    impl Read for Restart<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let bytes = match self.phase {
                0 => "\x1b]7501;state=done\x07".to_owned(),
                1 => {
                    let token = prepare(&self.fixture.home, &self.fixture.shell).unwrap();
                    format!(
                        "\x1b]7501;state=done\x07\x1b]7501;state=clear:somtus={token}\x1b\\\x1b]7501;state=working\x07"
                    )
                }
                _ => return Ok(0),
            };
            self.phase += 1;
            out[..bytes.len()].copy_from_slice(bytes.as_bytes());
            Ok(bytes.len())
        }
    }
    let f = Fixture::new();
    let token = prepare(&f.home, &f.shell).unwrap();
    collect(
        &f.home,
        &f.shell,
        &token,
        Restart {
            fixture: &f,
            phase: 0,
        },
        Vec::new(),
    )
    .unwrap();
    let saved = snapshot(&f.home, &f.shell).unwrap();
    assert_ne!(saved.generation, token);
    assert_eq!(saved.root.unwrap().state, State::Working);
}

#[test]
fn reported_state_overlays_activity_and_clear_restores_hook_fallback() {
    use crate::activity::{ActivityTracker, AgentActivity};
    let f = Fixture::new();
    let session = f.session(Some("claude"));
    fs::create_dir_all(f.home.join("agent-hooks/claude")).unwrap();
    fs::write(
        f.home.join(format!("agent-hooks/claude/{}.json", f.shell)),
        r#"{"session_id":"s","turn_id":"p","completed":true,"since_unix":50}"#,
    )
    .unwrap();
    let mut tracker = ActivityTracker::at(f.home.clone());
    let activity = |tracker: &mut ActivityTracker| {
        tracker.sample_states(std::slice::from_ref(&session), 100)[&f.shell].activity
    };
    assert_eq!(activity(&mut tracker), AgentActivity::Done);
    let mut reports = f.start();
    reports.apply(report("state=blocked:kind=permission"), 60);
    save(&f.home, &reports.value).unwrap();
    assert_eq!(activity(&mut tracker), AgentActivity::Waiting);
    reports.apply(report("state=clear"), 70);
    save(&f.home, &reports.value).unwrap();
    assert_eq!(activity(&mut tracker), AgentActivity::Done);
    reports.apply(report("state=working"), 80);
    save(&f.home, &reports.value).unwrap();
    assert_eq!(activity(&mut tracker), AgentActivity::Working);
    reports.value.connected = false;
    save(&f.home, &reports.value).unwrap();
    assert_eq!(
        activity(&mut tracker),
        AgentActivity::Unknown,
        "lost transport cannot revive the older hook completion"
    );
    fs::write(
        f.home.join(format!("agent-hooks/claude/{}.json", f.shell)),
        r#"{"session_id":"s","turn_id":"new","completed":true,"since_unix":90}"#,
    )
    .unwrap();
    assert_eq!(
        activity(&mut tracker),
        AgentActivity::Done,
        "a newer explicit hook fact is still authoritative"
    );
    reports.value.connected = true;
    save(&f.home, &reports.value).unwrap();
    let mut plain = f.session(None);
    plain.alive = true;
    assert_eq!(
        tracker.sample_states(std::slice::from_ref(&plain), 100)[&f.shell].activity,
        AgentActivity::Working
    );
    plain.alive = false;
    assert_eq!(
        tracker.sample_states(&[plain], 100)[&f.shell].activity,
        AgentActivity::Exited
    );
}
