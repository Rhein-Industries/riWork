# Exact-tty real PTY integration record

`verification_environment`: explicit isolated OS-temporary RIWORK_HOME, dedicated tmux server/socket and test-created sessions only. No live windows or live RiWork state.

The worker ran:

```sh
RIWORK_HOME=/var/folders/bb/kghlxdk94hj1nkcmwsr90r2c0000gn/T/riwork-terminal-detach-kbb1alae \
  cargo test -p riwork-engine --test terminal_detach -- --ignored --nocapture
```

Observed result: `exact_tty_detach_preserves_other_client_and_both_sessions` passed; 1 passed, 0 failed, exit 0. The fixture cleaned its owned socket/sessions and the root was empty afterward. This record summarizes the tool response recorded in the session. A standalone raw output log was not retained and is not fabricated here.

The test creates two actual openpty-backed clients, rejects a wrong-session tty and malformed tty, detaches exactly the requested client while preserving the other, confirms an idempotent repeated detach, and verifies session/pane survival. Cleanup guards only target resources created by this test. It is ignored by ordinary suites and requires an explicit RIWORK_HOME whose basename starts `riwork-terminal-detach-` beneath the OS temporary directory.

The separately captured `exact-tty-fake.log` proves the bounded fake-tmux/readback regression passed. The captured log identifies the exact regression test and its passing result; the root report owns the original command arguments and fixture environment, which are not embedded in that log.

Test source SHA256: `cb905366544a8e560dc646ef37423911527a2496253c5b065aa547ea8bf87f17`.
