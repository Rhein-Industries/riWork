//! `shell output`'s hash and change-detection loop, against scripted captures
//! (no tmux). The same loop against a real private tmux server is in
//! `tmux_tests`.
use super::*;
use std::time::Instant;

fn capture(output: &str) -> Capture {
    Capture {
        output: output.to_owned(),
        screen: Some(Screen {
            cursor: Cursor { x: 2, y: 0 },
            rows: 2,
            cols: 40,
            in_mode: false,
            history_size: 0,
            alternate: false,
        }),
    }
}

fn query(if_changed: Option<&str>, wait_ms: u64) -> OutputQuery<'_> {
    OutputQuery {
        lines: 200,
        styled: false,
        if_changed,
        wait: Duration::from_millis(wait_ms),
    }
}

/// A capture source that answers `same` until it has been asked `after`
/// times, then `later`, and a pause that sleeps for real.
struct Script {
    same: Capture,
    later: Capture,
    after: usize,
    captures: usize,
    pauses: Vec<Duration>,
}
impl Script {
    fn new(after: usize) -> Self {
        Self {
            same: capture("hi\n\n"),
            later: capture("hi there\n\n"),
            after,
            captures: 0,
            pauses: Vec::new(),
        }
    }
    fn hash(&self) -> String {
        output_hash(200, false, &self.same)
    }
    fn run(&mut self, query: &OutputQuery<'_>) -> Result<OutputRead, String> {
        let (same, later, after) = (self.same.clone(), self.later.clone(), self.after);
        let captures = &mut self.captures;
        let pauses = &mut self.pauses;
        poll_output(
            query,
            |pause| {
                pauses.push(pause);
                std::thread::sleep(pause);
            },
            || {
                *captures += 1;
                Ok(if *captures > after {
                    later.clone()
                } else {
                    same.clone()
                })
            },
        )
    }
}

#[test]
fn hash_is_sixteen_hex_digits_and_is_pinned_across_builds() {
    let hash = output_hash(200, false, &capture("hi\n\n"));
    assert_eq!(hash.len(), 16);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    // The value is part of the protocol: a hash a phone kept must still match
    // after the desktop restarts or updates. Change the algorithm only with a
    // new version tag, never silently (v2 added `history_size` and
    // `alternate`; a v1 hash simply never matches again).
    assert_eq!(hash, "b0cf30d132bb77f2");
    assert_eq!(hash, output_hash(200, false, &capture("hi\n\n")));
}

#[test]
fn hash_covers_text_screen_lines_and_styling() {
    let base = capture("hi\n\n");
    let hash = |lines, styled, capture: &Capture| output_hash(lines, styled, capture);
    let reference = hash(200, false, &base);
    let mut seen = HashSet::from([reference.clone()]);
    let mut differs =
        |value: String| assert!(seen.insert(value), "hash collision between variants");

    differs(hash(200, true, &base));
    differs(hash(199, false, &base));
    differs(hash(200, false, &capture("hi!\n\n")));
    differs(hash(200, false, &capture("hi\n\n\n")));
    for change in [
        |s: &mut Screen| s.cursor.x += 1,
        |s: &mut Screen| s.cursor.y += 1,
        |s: &mut Screen| s.rows += 1,
        |s: &mut Screen| s.cols += 1,
        |s: &mut Screen| s.in_mode = true,
        |s: &mut Screen| s.history_size += 1,
        |s: &mut Screen| s.alternate = true,
    ] {
        let mut other = base.clone();
        change(other.screen.as_mut().unwrap());
        differs(hash(200, false, &other));
    }
    differs(hash(
        200,
        false,
        &Capture {
            screen: None,
            ..base.clone()
        },
    ));
    // Text moving between fields must not produce the same hash: one field
    // ending is not another beginning.
    let a = Capture {
        output: "ab".into(),
        screen: None,
    };
    let b = Capture {
        output: "a".into(),
        screen: None,
    };
    assert_ne!(hash(1, false, &a), hash(1, false, &b));
    // And nothing else matters: the same answer, however it was made.
    assert_eq!(reference, hash(200, false, &capture("hi\n\n")));
}

#[test]
fn without_if_changed_there_is_one_capture_and_never_a_pause() {
    let mut script = Script::new(usize::MAX);
    let read = script.run(&query(None, 5_000)).unwrap();
    let hash = script.hash();
    assert_eq!(
        read,
        OutputRead::Changed {
            capture: capture("hi\n\n"),
            hash
        }
    );
    assert_eq!((script.captures, script.pauses.len()), (1, 0));
}

#[test]
fn a_different_hash_returns_the_content_at_once() {
    let mut script = Script::new(usize::MAX);
    let started = Instant::now();
    let read = script.run(&query(Some("0000000000000000"), 5_000)).unwrap();
    assert!(matches!(read, OutputRead::Changed { .. }));
    assert_eq!((script.captures, script.pauses.len()), (1, 0));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_same_hash_with_no_wait_is_unchanged_after_one_capture() {
    let mut script = Script::new(usize::MAX);
    let hash = script.hash();
    let read = script.run(&query(Some(&hash), 0)).unwrap();
    // The screen comes along: `history_size` and `alternate` are still told.
    assert_eq!(
        read,
        OutputRead::Unchanged {
            hash,
            screen: script.same.screen
        }
    );
    assert_eq!((script.captures, script.pauses.len()), (1, 0));
}

#[test]
fn a_change_ends_the_wait_early() {
    // The screen changes on the fourth capture, three pauses in.
    let mut script = Script::new(3);
    let hash = script.hash();
    let started = Instant::now();
    let read = script.run(&query(Some(&hash), 8_000)).unwrap();
    let elapsed = started.elapsed();
    let OutputRead::Changed { capture, hash: new } = read else {
        panic!("expected a change");
    };
    assert_eq!(capture.output, "hi there\n\n");
    assert_ne!(new, hash);
    assert_eq!(new, output_hash(200, false, &capture));
    assert_eq!(script.captures, 4);
    assert_eq!(script.pauses, vec![OUTPUT_POLL; 3]);
    assert!(elapsed >= OUTPUT_POLL * 3);
    assert!(
        elapsed < Duration::from_secs(4),
        "{elapsed:?} for an 8 s wait"
    );
}

#[test]
fn no_change_returns_unchanged_when_the_wait_is_over_and_not_much_later() {
    let mut script = Script::new(usize::MAX);
    let hash = script.hash();
    let started = Instant::now();
    let read = script.run(&query(Some(&hash), 400)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(
        read,
        OutputRead::Unchanged {
            hash,
            screen: script.same.screen
        }
    );
    assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(400) + Duration::from_secs(2));
    // Captured about every 80 ms; never a pause longer than that, and the
    // pauses fill the wait without running past it.
    assert!(script.pauses.iter().all(|pause| *pause <= OUTPUT_POLL));
    assert!(script.pauses.iter().sum::<Duration>() <= Duration::from_millis(400));
    assert!((3..=7).contains(&script.captures), "{}", script.captures);
    assert_eq!(script.captures, script.pauses.len() + 1);
}

#[test]
fn the_wait_never_exceeds_the_cap() {
    let start = Instant::now();
    assert_eq!(output_deadline(start, Duration::ZERO), start);
    assert_eq!(
        output_deadline(start, Duration::from_millis(1234)) - start,
        Duration::from_millis(1234)
    );
    assert_eq!(
        output_deadline(start, Duration::from_secs(10)) - start,
        MAX_OUTPUT_WAIT
    );
    assert_eq!(
        output_deadline(start, Duration::MAX) - start,
        MAX_OUTPUT_WAIT
    );
    assert_eq!(MAX_OUTPUT_WAIT, Duration::from_secs(10));
    assert_eq!(OUTPUT_POLL, Duration::from_millis(80));
}

#[test]
fn a_capture_error_ends_the_wait_with_that_error() {
    let mut calls = 0;
    let hash = output_hash(200, false, &capture("hi\n\n"));
    let result = poll_output(&query(Some(&hash), 8_000), std::thread::sleep, || {
        calls += 1;
        if calls < 3 {
            Ok(capture("hi\n\n"))
        } else {
            Err("shell 1 has exited".to_owned())
        }
    });
    assert_eq!(result.unwrap_err(), "shell 1 has exited");
    assert_eq!(calls, 3);
}

#[test]
fn the_lines_in_the_hash_are_the_clamped_ones() {
    // `--lines 0` and `--lines 1` capture the same thing, so they hash alike.
    let same = capture("hi\n\n");
    let ask = |lines| {
        let query = OutputQuery {
            lines,
            ..query(None, 0)
        };
        poll_output(&query, std::thread::sleep, || Ok(same.clone())).unwrap()
    };
    assert_eq!(ask(0), ask(1));
    assert_eq!(ask(HISTORY_LINES), ask(HISTORY_LINES + 5));
    assert_ne!(ask(1), ask(2));
}
