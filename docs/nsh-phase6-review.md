# NSH Phase 6d: design and security review

```yaml
assignment: dbd40e1a-36f6-4721-b419-aea7fa682e31
umbrella: 6ef6a636 (Phase 6)
branch: nsh-p4-chat at nsh 4df3822 (fast-forward; review adds this file only)
scope: review only, no code changes
verification_environment: source_and_saved_evidence_review_only
live_targets_touched: none
```

Owner lanes used below: **6a** settings (`12c69ae4`), **6b** schedules/remote (`82d585f6`),
**6c** cua/previews/host (`7b18aada`), **chat** (the chat lane).

## Method

- Reviewed the current source at `4df3822` (read-only) together with the contracts and the parity
  checklist. The live product is the `riwork` binary from `crates/riwork-desktop/src/nsh_main.rs`
  (`autobins = false`, `crates/riwork-desktop/Cargo.toml:6,11-13`) with its `nsh_*.rs` modules,
  `crates/riwork-nsh-native`, `crates/riwork-nsh-workspace`, `crates/riwork-nsh-terminal`,
  `apps/riwork`, and the shared engine and core crates. `crates/riwork-desktop/src/main.rs` and
  the modules it declares are the uncompiled GPUI reference. They are used only to state what a
  port must preserve.
- Four areas were read by independent reviewers (attachments, remote input, Codex accounts,
  schedule dispatch), and a fifth reconciled the parity and phase documents. Dictation was
  reviewed directly. Each High or Medium finding was re-read against the source at
  `4df3822` before it went in here, and the references for Low and Info findings were checked line by line.
  Where a reviewer's line was off, the corrected line is given.
- Nothing was built, run, paired or opened. No app window, live chat or live state was used.
- Evidence rule: nothing here is called done on GUI evidence alone. The screenshots in
  `docs/verification/` (`schedules-*.png`, `project-codex-*.png`, `automations/`) date from
  2026-09-27 to 2026-10-06, before the NSH host replaced the desktop binary (`e00ce39`,
  2026-10-27). They show the GPUI app and are not NSH evidence.

Severity: **Critical**, exploitable or data-losing in the live NSH path now. **High**, a security,
integrity or billing defect reachable now, or a gap that silently removes a working feature.
**Medium**, a missing guard or contract gap with a plausible failure. **Low**, hardening or a
documentation fix. **Info**, by design or for a decision.

Status: **live** (in the NSH build now), **port** (a requirement the Phase 6 port must meet),
**contract** (a gap in `docs/nsh-contracts.md`), **doc** (docs disagree with code or with each other).

## Summary

| Severity | Count | IDs |
|---|---:|---|
| Critical | 0 | |
| High | 2 | SCH-1, ACC-1 |
| Medium | 19 | SCH-2..4, REM-1..4, ACC-2..5, ATT-1..7, DIC-1 |
| Low | 20 | SCH-5..8, REM-5..9, ACC-6..10, ATT-8..12, DIC-2 |
| Info | 5 | REM-10, REM-11, ACC-11, ATT-13, DIC-3 |

| Lane | High | Medium | Low | Info |
|---|---:|---:|---:|---:|
| 6a settings | 0 | 2 | 2 | 0 |
| 6b schedules/remote | 1 | 5 | 6 | 1 |
| 6c cua/previews/host | 0 | 5 | 8 | 0 |
| chat | 1 | 7 | 4 | 4 |

Each row's Status cell names its lane. Where it names two, the count uses the first.

Headlines:

- **SCH-1:** the NSH build dispatches no schedules. The only caller of `tick_tracked` outside the
  engine is the uncompiled GPUI ticker, and the dashboard still shows automations as "due now".
- **ACC-1:** a delegated worker of a Codex chat bound to the system default can run on a
  different, possibly balanced, account. This is in the shared engine, so GPUI has it too.
- **No paired-Mac path is wired into the NSH GUI yet.** The remote findings concern the host's
  own inputs and the requirements the Phase 6 remote port must meet.
- **ATT-1:** the Phase 4a submit path can drop a GPUI-era draft's attachments, or leave
  already-sent text to be restored. This is a chat-lane defect in the 4a bridge.

## Schedules and dispatch authority (6b)

| ID | Sev | Status | Finding and failure | References | Fix direction |
|---|---|---|---|---|---|
| SCH-1 | High | live (6b) | **No dispatcher in the NSH build.** The host starts only notification claims and orchestrator wakes. When only NSH and the CLI run, saved automations stop with no outcome recorded. The CLI and MCP only create, read, update and delete (`schedule_service.rs:1-2`). Schedules fire only if an old GPUI process still runs on the same home. Phase 1 lists this as D-SVC "Not started", but Phase 5 does not carry it as a gap and the gap register has no entry. | `crates/riwork-desktop/src/nsh_host.rs:2342-2344`; only caller `crates/riwork-desktop/src/schedules.rs:19` (uncompiled); `crates/riwork-engine/src/schedules.rs:732`; contract `docs/nsh-contracts.md:317-319`; `docs/nsh-phase1-acceptance.md:694`; parity SHELL-238 `docs/nsh-parity.md:459` | Host-internal `ScheduleTicker` beside `notices::start`, meeting SCH-4. Until then, record the gap (gap register, Phase 6 docs) and apply SCH-2's interim wording. |
| SCH-2 | Medium | live, doc (6b) | **Surfaces promise dispatch.** Dashboard rows say "next in …" and then "due now" indefinitely. "Needs you" says "no automation failed". The Automations panel shows "This view is unavailable.", so the user can't pause or inspect. MCP and CLI text say dispatch happens "while the desktop app is open", which is true of GPUI only. | `apps/riwork/dashboard/model.ts:266,274`; `apps/riwork/DeferredPanel.tsx:28`; `crates/riwork-cli/src/mcp.rs:139`; `crates/riwork-cli/src/cli.rs:340` | Show "not dispatched by this build" on rows and the panel placeholder; reword MCP and CLI. Once a ticker exists, publish its status on the contract's `runtime` topic. |
| SCH-3 | Medium | live; engine, shown by NSH (6b) | **Failed runs are hidden from attention.** A Failed or Uncertain run always pauses the schedule, and the attention scan skips paused schedules. So a run that needs review never appears, and the summary says "no automation failed". Saving the schedule to acknowledge it unpauses it and keeps `last_run`, and only then does the stale failure appear. The test builds a failed-and-unpaused state that dispatch never produces. | `crates/riwork-engine/src/schedules.rs:916-917,946-947,654`; `crates/riwork-engine/src/global_overview.rs:596-603`; test `crates/riwork-engine/src/global_overview/tests.rs:648-656` | Drive attention from `review_required` (or paused with Failed/Uncertain). Mark the outcome acknowledged on save. Fix the fixture. |
| SCH-4 | Medium | port (6b; 6c host lifecycle) | **Ticker requirements.** (a) A reload or quit during a delivery leaves `Dispatching`, which the next owner turns into Uncertain and paused. That is safe, but a routine reload pauses automations. (b) A ticker in a fixture home or under `RIWORK_NSH_BACKGROUND=1` would start a real chat host through `orchestrators::system_ensure`, because the chat path is injectable only under `cfg(test)`. That breaks the Phase 5 fixture rule. (c) `schedules.lock` is held for the whole delivery (fresh chat: about 2 + 10 + 10 s plus host start), which blocks every `list()`, including the dashboard reader. | `crates/riwork-engine/src/schedules.rs:536-542,793-813`; `crates/riwork-engine/src/schedule_chat.rs:45`; `crates/riwork-desktop/src/nsh_runtime.rs:66-120,172-195` | Start only in `nsh_host::run` while holding `desktop.lock`, on its own thread, ticks never overlapping. On quit or reload, stop claiming and wait a bounded time for the in-flight tick. Make chat delivery injectable outside tests and skip fixture homes. Keep `Schedules.*` async. |
| SCH-5 | Low | doc (6b) | **Who starts dispatch is stated three ways**: host-internal (contracts), `Schedules.startDispatcher` (SHELL-238), `Scheduler.start` (DOCS-205). Following a parity row would start dispatch from TSX, once per window. | `docs/nsh-contracts.md:150,317-319,486-487`; `docs/nsh-parity.md:459,2688` | Settle on host-internal with status on the `runtime` topic; update both rows. |
| SCH-6 | Low | contract (6b) | **§1.8 cannot do what the panel needs**: options-only edits (including permission), explicit rebind, target listing. Without these the bridge would edit `Target` and call the store directly, outside the validated engine path. | `docs/nsh-contracts.md:323-329`; `crates/riwork-engine/src/schedule_service.rs:255-264`; reference `crates/riwork-desktop/src/schedule_panel.rs:202-214`; parity PANEL-108/109/112 `docs/nsh-parity.md:2174-2178` | Engine methods for options update (with `validate_new_chat`), target listing and rebind; extend §1.8. |
| SCH-7 | Low | port (6b) | **Unattended Full-access runs can be arranged from the CLI or MCP** with no confirmation, and nothing records who created the schedule. Dispatch-time validation checks root, account and harness, not permission or origin. A project orchestrator runs Full by design. The same MCP server already runs arbitrary commands immediately, so this adds persistence, not a new privilege. | `crates/riwork-cli/src/mcp.rs:992` (permission enum); `crates/riwork-engine/src/orchestrators.rs:446-450`; `crates/riwork-engine/src/schedules.rs:207-252` | Record and show provenance (UI, CLI, MCP). Consider UI confirmation before arming a CLI- or MCP-created Full or AutoEdit schedule. |
| SCH-8 | Low | port (6b) | **Prompt hygiene is checked only on save.** `read()` validates ids and timing; the control-character and length checks run only in `save()`. A ledger written another way could paste ESC or CR into an agent pane. Unverified: a one-line `!` prompt sent to a Claude Code pane may enter its bash mode. | `crates/riwork-engine/src/schedules.rs:566-574,631-636` | Repeat `single_line` and the length checks at read or dispatch, failing closed. Decide on `!` and `/` prompts for terminal targets after checking agent paste behaviour. |

Checked and sound: at-most-once delivery across GPUI, NSH, CLI and reload overlap, via a
non-blocking exclusive lock, a durable claim before input, and Dispatching turning into Uncertain
plus a pause with no retry (`schedules.rs:793-813,853-895`). No backlog storm: a 300 s grace
period, one claim per tick, older runs marked Missed, and at most 4 claims per 60 s (`schedules.rs:17,814-864`).
Paused and deleted schedules never fire (`schedules.rs:819-821`). Identity is re-validated at fire
time for terminal, new-chat and orchestrator targets (`sessions.rs:1682-1820`,
`schedule_chat.rs:302-315`). The ledger is 0600, refuses symlinks, is written atomically and is
capped (`schedules.rs:554-596,959-978`). No remote path creates schedules.

## Remote and other untrusted input (6b, 6c)

No paired-Mac feature is wired into the NSH GUI. The host's inputs are the desktop socket,
argv, host calls, notification clicks, the restore ticket, terminal output and the CLI.

| ID | Sev | Status | Finding and failure | References | Fix direction |
|---|---|---|---|---|---|
| REM-1 | Medium | live (6c) | **A relative folder forwarded to a running instance is resolved in that instance's working directory.** `riwork .` passes the CLI's `is_dir` check in the caller's directory. The raw argv is forwarded, and the receiver canonicalizes it in its own directory (`/` when launched from Finder). The result can be `/` registered as a project and agents starting there. The comment that the path "has already been resolved by the CLI" is wrong. | `crates/riwork-desktop/src/nsh_main.rs:39`; `crates/riwork-desktop/src/nsh_host.rs:1780-1786,2148,1713-1714`; `crates/riwork-desktop/src/nsh_runtime.rs:35,306-315`; `crates/riwork-cli/src/cli.rs:553-558` | Make the path absolute in the sender. Reject non-absolute paths in `read_request`. Test a relative forward. |
| REM-2 | Medium | live, contract (6c) | **The privileged TSX bundle is read from the build checkout**: `CARGO_MANIFEST_DIR/../../apps/riwork/.nsh/out/bundle.js`. `RIWORK_NSH_BUNDLE` overrides it in release builds too. Anything that rewrites that file (for example an `nsh build` in the main checkout) replaces the live app's UI code at the next start. That code can approve tool requests or set Full approval. `RIWORK_NSH_BACKGROUND` is also honoured in release builds. Same user, so this is integrity rather than escalation. | `crates/riwork-desktop/src/nsh_bundle.rs:5-18`; `crates/riwork-desktop/src/nsh_host.rs:2289-2290`; `docs/nsh-contracts.md:61` | Ship the bundle inside the app with a build-time hash. Gate both variables behind debug or fixture builds. State the bundle trust root in contracts §0. |
| REM-3 | Medium | port, contract (6b) | **Ids and strings from a paired host are not validated by the client.** Shell ids are any string, and only the 16 MiB line cap bounds them. Saved remote tabs check only non-empty, despite the comment saying UUID. The `{kind:"remote"}` terminal target has no rule for UUID ids or for argv without a shell. The GPUI reference quotes correctly, but a port that builds a shell string would allow command injection from a compromised host. | `crates/riwork-engine/src/remote_tree.rs:1391-1393`; `crates/riwork-engine/src/remote_hosts.rs:49`; `crates/riwork-core/src/layouts.rs:1345-1353`; `docs/nsh-contracts.md:692`; parity TERM-090 `docs/nsh-parity.md:827`; reference `crates/riwork-desktop/src/main.rs:1690-1700` | Canonical UUIDs for host and peer ids. Bound names, paths and cwd, and strip control characters. Add to §3.2: the remote target spawns argv directly and rejects non-UUID ids. |
| REM-4 | Medium | live (standalone `riwork-remote`, 6b) | **`riwork-remote pair` defaults to protocol 1**, the long-lived PSK with no forward secrecy. Anyone who sees a v1 link once keeps access until it is revoked. The app's own pairing passes `--protocol 2`. | `remote/src/main.rs:45-47`; `crates/riwork-engine/src/remote_hosts.rs:220` | Default to 2. Keep 1 behind an explicit flag. |
| REM-5 | Low | live (6c) | **`desktop.sock` is weaker than `chat.sock`.** `create_private_dir` accepts an existing directory without checking owner or mode, and the socket has no peer-uid check. Exploiting this needs an already loose `RIWORK_HOME/run`. | `crates/riwork-core/src/paths.rs:39-41`; `crates/riwork-desktop/src/nsh_runtime.rs:73,217-220`; compare `crates/riwork-engine/src/chat/host.rs:205-214` | Reuse the chat host's run-directory check and peer-uid check. |
| REM-6 | Low | live, doc (6c) | **`system.openWith` re-checks less than GPUI did**, and takes an `app` and URLs that the contract does not list. `open_refusal` follows symlinks but takes the extension from the link name, with no root confinement. A future caller passing an uncanonicalized `notes.md` that points to an `.app` would launch it. Today's callers are safe (terminal links are canonicalized; Files uses `validateOpen`). | `crates/riwork-desktop/src/nsh_host.rs:1400-1406,1414-1430`; `crates/riwork-engine/src/terminal_links.rs:1180-1201` | Canonicalize or refuse symlinks in the host before `open_refusal`. Drop or allow-list `app`. Document URLs. |
| REM-7 | Low | doc (6c) | **The contract's `openUrl` wording inverts NSH's API.** It says allowed URLs are opened and "declines everything else", but in NSH `decline()` means the system opens the URL. The code is right (`decline` for allowed, `handled` otherwise), so a "fix" to match the doc would open every scheme. Refused URLs also report success to TSX. | `docs/nsh-contracts.md:598`; `crates/riwork-desktop/src/nsh_host.rs:675-681` | Reword to "answers handled (swallows) everything else". Consider surfacing refusals. |
| REM-8 | Low | live (6c; 6b for remote) | **OSC 52 clipboard writes from terminal output are silent**, as the contract allows. Agent output, a `cat` of a hostile file, or later a compromised paired host can replace the clipboard. | `crates/riwork-nsh-terminal/native/terminal.m:191-201`; `docs/nsh-contracts.md:741` | Allow writes only from a focused surface after recent input, or behind a setting. Refuse from remote surfaces by default. |
| REM-9 | Low | live (standalone, 6b) | **Peer text goes to the CLI as argv** (chat commands, `shell create --command`), although the crate itself notes that argv is visible in the process list. Whether other users can see it on macOS is unverified. Cua's installer is also an unpinned `install.sh` over https; the Team ID check runs only afterwards. | `remote/src/rpc/chat.rs:1683-1686`; `remote/src/rpc.rs:587-589,1218-1219`; `crates/riwork-engine/src/cua.rs:29,498-521,901` | Use the existing stdin path. For Cua (6c), keep the post-install identity check and pin the script or use a signed package. |
| REM-10 | Info | design (6b, chat) | **A paired device has the host user's full power**: arbitrary commands, unrestricted agents, approvals and Full mode. No per-device scope, no host-side notice. | `remote/src/rpc.rs:581-589`; `remote/src/rpc/chat.rs:59-65` | Optional per-device scopes and an audit or notice in Settings → Remote. |
| REM-11 | Info | decision (chat) | **Any same-uid process can escalate a chat to Full** through `riwork chat command`, while billing changes from commands are refused ("chosen in RiWork's window"). An agent that can run the CLI already has a shell, so the extra capability is small. | `crates/riwork-cli/src/cli.rs:2807-2826`; `crates/riwork-cli/src/cli/chat_remote.rs:467`; `crates/riwork-engine/src/chat/model.rs:26-28` | Decide whether escalating approval mode is window-only, like billing. |

Checked and sound:
- **Host calls.** `window.open` uses strict decoding and validation, and route steps are validated
  (UUIDs, absolute editor paths, bounded lines). Unknown operations are rejected, and calls from
  quitting or foreign windows are refused (`nsh_host.rs:101-143,452-466,1410,1452-1484`).
- **`system.notify` and completion clicks.** `system.notify` is bounded, and completion clicks
  are verified (`nsh_notifications.rs:61-147,166-182`).
- **Desktop socket requests.** Strictly decoded, 64 KiB cap, timeouts (`nsh_runtime.rs:37-41,306-315`).
- **Restore ticket.** A UUID with expiry, executable and home checks (`runtime.rs:577-590,1989-1995`).
- **Fixture switches.** Release-gated (`nsh_notifications.rs:265-279`, `files.rs:1220-1235`,
  `chat/service.rs:61-64,142-150`).
- **Terminal.** URL opening, OSC title and notifications are swallowed, and OSC 52 reads are
  refused (`terminal.m:178-190`).
- **Chat socket.** `chat.sock` checks the directory and peer uid (`chat/host.rs:201-227,1328`).
- **Cua guard.** Refuses input to RiWork itself, and the NSH pid is registered
  (`cua/guard.rs:107-145`, `runtime.rs:314-335`).
- **Phase 6 port requirements carried from the reference.**
  - Keep `remote:` namespacing and the UUID gate for local projects (`nsh_host.rs:330-333,1709-1710`).
  - Keep pairing links on stdin and never persisted (`remote_hosts.rs:164-180`).
  - Keep `RIWORK_REMOTE_BIN` and `RIWORK_CLI` dev-only (`remote_cli.rs:10-35`).
  - A saved `AppPreview` tab must never launch its command on restore (`preview_control.rs:1-4`, `app_preview.rs:21-23`).

## Codex account selection (6a, chat)

How the account is chosen today:
- A new Codex chat, orchestrator or terminal resolves its account in this order: explicit id,
  then the project pin, then the app default, or balancing when that is on and the project
  inherits (`chat/launch.rs:23-42`, `codex_balancing.rs:82-165`).
- The result is stored in `ChatInfo.codex_account_id` (`chat/host.rs:1847-1852`), and resume
  uses it without re-balancing (`launch.rs:46-71`).
- No NSH UI writes an account scope yet: `Accounts.selectDefault` has no TSX caller, and the
  status-bar account menu is read-only.

| ID | Sev | Status | Finding and failure | References | Fix direction |
|---|---|---|---|---|---|
| ACC-1 | High | live; engine (chat) | **A delegated worker of a system-default Codex parent picks an account as if it were new.** A system-default chat stores `None`. Delegation copies that `None`, and the launcher treats `None` as "resolve fresh" (project pin, app default or balancing). Workers then run and bill on a different account. Schedules handle the same case correctly by mapping `None` to `SYSTEM_DEFAULT_ID`. In NSH the trigger is a default or balancing change after the orchestrator was created; via the CLI it happens at once. The docs say workers keep the parent's account. | `crates/riwork-engine/src/chat/delegation.rs:329-333`; `crates/riwork-engine/src/chat/launch.rs:33`; `crates/riwork-core/src/codex_accounts.rs:146-154`; contrast `crates/riwork-engine/src/schedules.rs:229-234`, `crates/riwork-engine/src/schedule_chat.rs:260-263`; `docs/chat-delegation.md:23-24` | For a same-provider Codex child, map a parent `None` to `SYSTEM_DEFAULT_ID`. Check the child's account matches the request, as the CLI does (`crates/riwork-cli/src/cli.rs:2690-2702`). Add an engine test with balancing on. |
| ACC-2 | Medium | live (chat exposes, 6a consumes) | **The status bar shows the project default's account and quota for a focused chat.** Chat tabs carry no account and read as "other". A chat balanced onto C, or created before the pin moved, shows B's quota. GPUI used the chat's own account. Usage also never polls a chat's own home unless it is a listed account. | `apps/riwork/usage.ts:146-150`; `crates/riwork-nsh-native/src/chat/dto.rs:204-226`; `crates/riwork-nsh-native/src/usage.rs:582-616`; reference `crates/riwork-desktop/src/account_status.rs:588-605` | Add `codexAccountId` and provider to the chat panel or summary payload. Resolve focus from it. Add open chats' homes as usage targets. |
| ACC-3 | Medium | port (6a) | **The saved account list is never re-read after the first discovery**, and NSH runs discovery only when no list exists. Removed or added Orca accounts stay stale for pins, the default, balancing and usage polling. GPUI ran discovery at app start (PANEL-148). Phase 5 lists the missing refresh but not this safety effect. | `apps/riwork/StatusBar.tsx:95-97`; `crates/riwork-nsh-native/src/usage.rs:733-752`; `crates/riwork-core/src/codex_accounts.rs:144-163`; `docs/nsh-phase5-global.md:226` | Run background discovery at start. Add Settings' Refresh. Re-read `codex-accounts.json` when it changes. |
| ACC-4 | Medium | contract (6a) | **`Store.setProjectCodexAccount` is contracted but not implemented.** The only account write in `riwork/native` is the global `Accounts.selectDefault`. A port that wires the project menu to it would change the default for every inheriting project. | `docs/nsh-contracts.md:195`; absent from `crates/riwork-nsh-native/src/lib.rs`; `apps/riwork/StatusBar.tsx:183-186`; parity PANEL-189 `docs/nsh-parity.md:2295`; engine setter `crates/riwork-core/src/store.rs:1485-1492` | Add the Store method (`{source, accountId?}`, refusals as `refused:`). Use it from the menu and Project Settings. Test that `selected_codex_account` is untouched. |
| ACC-5 | Medium | contract (chat) | **`NewChatRequest` has no account field**, and `Chat.create` passes `None`. A new-chat dialog that previews "Codex · A" can't create on A. A pin change in another window between preview and confirm silently uses B. GPUI sent the previewed account (CHATC-175, CHATC-180). | `crates/riwork-nsh-native/src/chat/dto.rs:336-358`; `crates/riwork-nsh-native/src/chat/mod.rs:135`; reference `crates/riwork-desktop/src/chat_choice.rs:348-354`; parity `docs/nsh-parity.md:1868` | Add `codexAccountId?`: absent means automatic, `system-default` means explicit, an id means that account. Check the returned account. |
| ACC-6 | Low | live (engine, chat) | **The system default's home is not stored.** A system-default chat resumes under whatever `CODEX_HOME` the current host process has. A CLI-started host with an exported `CODEX_HOME` and a later Dock-started host differ. The effect on Codex resume is unverified. | `crates/riwork-engine/src/chat/launch.rs:66-71`; `crates/riwork-core/src/codex_accounts.rs:146-154`; compare `crates/riwork-engine/src/schedules.rs:241-249` | Store the resolved home at creation. Refuse resume if it changed. |
| ACC-7 | Low | doc (chat) | **The vanished-account error tells the user to choose in Settings**, which never changes an existing chat's binding. It fails safe. | `crates/riwork-core/src/codex_accounts.rs:304-313`; `crates/riwork-engine/src/chat/launch.rs:46-47` | A chat-specific message: restore the account in Orca, or hand off. |
| ACC-8 | Low | contract (6a) | **The API key surface has no NSH contract**, and its module name conflicts: contracts put it in `Accounts`, parity names `ApiKey.*`. The Rust side is sound. | `docs/nsh-contracts.md:344`; parity PANEL-165..169 `docs/nsh-parity.md:2261-2265`; `crates/riwork-engine/src/chat/launch.rs:94-113,152-188` | Freeze one name. Presence-only status, no read method, and errors never echo the key. |
| ACC-9 | Low | live (6a) | **`selectDefault` checks against the cached in-memory list**, not the disk. A deleted home can be saved as the default; launches then refuse. | `crates/riwork-nsh-native/src/usage.rs:982-998,733-752`; compare `crates/riwork-core/src/store.rs:1490-1492` | Check with `resolve_launch_binding(home, Some(id))`. |
| ACC-10 | Low | live (6c) | **Starting `riwork` directly from an account-bound RiWork shell keeps the injected `CODEX_HOME`**, which later harness shells inherit. `riwork open` strips it, and GPUI behaved the same. Hypothesis-level impact. | `crates/riwork-desktop/src/nsh_startup.rs:32-39`; `crates/riwork-cli/src/cli.rs:939-942`; `crates/riwork-engine/src/sessions.rs:2342-2347` | Strip the injected variables in `nsh_startup` before threads start. |
| ACC-11 | Info | matches GPUI (chat) | **Switching a chat away from Codex and back picks the Codex account again** (current default or balanced), and the UI does not show which. | `crates/riwork-engine/src/chat/host.rs:3409-3410,3442`; `apps/riwork/chat/ChatTab.tsx:471-474` | Show the account in the toolbar once ACC-2's field exists. |

Checked and sound:
- **No silent fallback.** A bad choice refuses rather than falling back to the system account;
  balancing honours project pins and refuses when no account has quota
  (`codex_accounts.rs:142-163,304-319`, `codex_balancing.rs:87-101,159-164`).
- **Saved homes are used on resume and reload.** Resume uses the saved id, and terminal reloads
  use the saved home (`session_reload.rs:690-713`).
- **Scheduled chats** pin both the account and the home (`schedules.rs:174-251`).
- **No leaks.** The dashboard wire carries no account, home or email. Usage and discovery errors
  are fixed text. The balancing cache is 0600, holds no labels and does not follow symlinks
  (`codex_balancing.rs:345-357,433-441`).
- **Environment and Keychain.** The host spawn strips injected variables, and startup asks the
  Keychain nothing (`chat/host.rs:2127-2133,3623-3638`).
- **Settings writes** touch only their own keys (`settings.rs:314-340`, `usage.rs:273-285`).

## Uploads, attachments, paste and drop (chat, 6c)

What reaches JS today:
- No attachment staging (`crates/riwork-nsh-native/src/chat/mod.rs:100-176`).
- `system.readPasteboard` is rejected as `unavailable` (`nsh_host.rs:661-674`).
- Terminal paste and drop are native: `riwork.terminal` in `crates/riwork-nsh-terminal`.

| ID | Sev | Status | Finding and failure | References | Fix direction |
|---|---|---|---|---|---|
| ATT-1 | Medium | live (chat; Phase 4a) | **Submit ignores a draft's attachments and references.** `submit` snapshots text only, while `saveDraft` carries a GPUI-era draft's attachments forward under the composer's owner. An edited send therefore goes out as text only, and the owner match clears the attachments. An unedited send never matches, so the stored draft (text and attachments) stays, and the sent text comes back on the next restore. That invites a duplicate send. `DraftDto` has no attachment field, so the user is never told. | `crates/riwork-nsh-native/src/chat/mod.rs:368-371,652-666`; `crates/riwork-nsh-native/src/chat/drafts.rs:375-378`; `crates/riwork-nsh-native/src/chat/dto.rs:454-458`; `apps/riwork/chat/ChatTab.tsx:433,503` | Until attachments land, refuse or warn when the stored draft has attachments or references. Never clear them unless the snapshot carries them. Report their count in `DraftDto` and `SavedSubmissionDto`. |
| ATT-2 | Medium | live (chat) | **User messages lose their attachment lines unconditionally.** A GPUI message sent with files renders as bare text, or as an empty bubble if it had files only. Legacy messages lose their lines too (CHATC-120 says they show as before). Typed text that matches the pattern is hidden. | `crates/riwork-nsh-native/src/chat/view.rs:990-1002`; reference `crates/riwork-desktop/src/chat_view/attachment_ui/sent.rs:35-42`; parity CHATT-053 `docs/nsh-parity.md:1102`, CHATC-120 `:1788` | Strip only when `presentation.attachments` is non-empty (`attachments::typed`). Until cards exist, emit an `attachments` row field. |
| ATT-3 | Medium | live, contract (6c) | **A terminal text paste skips Ghostty's unsafe-paste protection.** Every ⌘V of text goes to `tmux paste-buffer -p` unchecked. Hostile multi-line clipboard text in a pane without bracketed paste (bash 3.2, `sh`, a REPL) runs line by line. GPUI went through Ghostty's paste and dropped unsafe pastes (TERM-036). Open question: does tmux strip `ESC[201~`? | `crates/riwork-nsh-terminal/native/terminal.m:632-634`; `crates/riwork-nsh-terminal/src/macos.rs:479`; `docs/nsh-contracts.md:759-760`; parity TERM-036 `docs/nsh-parity.md:738` | Add a native unsafe-paste rule before `Input::Paste`: refuse or confirm newlines when bracketed paste is off; always strip or refuse the paste-end marker and C0/C1 controls. Record it in §3.4. |
| ATT-4 | Medium | live (6c) | **A pasted or dropped TIFF is decoded and re-encoded on the main thread** with only a 24 MiB compressed-size cap. A TIFF declaring a huge raster can freeze or crash every window. The chat path has a bounded admission step (8192 px, 16 MP, 64 MiB) that the terminal does not use. | `crates/riwork-nsh-terminal/native/terminal.m:637-644`; compare `crates/riwork-engine/src/chat/attachments/clipboard_image.rs:12-37` | Send the capped raw bytes to Rust, admit them with the bounded normaliser off the main thread, then write the PNG. |
| ATT-5 | Medium | contract (6c host, chat consumer) | **The `system.readPasteboard` contract leaves the temp image unspecified**: location, permissions, cleanup, per-type caps. TSX can't delete files, so clipboard screenshots pile up. A JS `maxBytes` replaces the host's per-type caps. Parity says to keep bytes in a native handle and the reader in Rust (CHATC-102..104). | `docs/nsh-contracts.md:592`; parity `docs/nsh-parity.md:1755,1760-1762`; `crates/riwork-desktop/src/nsh_host.rs:661-674` | Return an opaque native staging handle consumed by `Chat.stageAttachment`. Alternatively, specify a 0700 directory with 0600 `O_NOFOLLOW` files, deletion after staging or by TTL, a startup sweep, and host ceilings that JS can only lower. |
| ATT-6 | Medium | contract (chat, 6c mints sources) | **`Chat.stageAttachment(chatId, file)` lets JS name any readable file.** The host copies it into the chat, and the next send gives it to the provider. A TSX bug, or turning an agent-printed path into a stage call, would leak data silently. This contradicts §0 ("TSX has no filesystem … capabilities"). | `docs/nsh-contracts.md:61,218,283`; `crates/riwork-engine/src/chat/client.rs:187-197`; `crates/riwork-engine/src/chat/host.rs:1473-1481` | Accept only host-minted sources (open panel, drop, pasteboard tokens). Deny `RIWORK_HOME` except designated staging. |
| ATT-7 | Medium | port (chat) | **Staging reads and decodes the source under the chat's main lock.** A dataless iCloud file or a stalled mount blocks that chat's streaming and every `Chat.list`. | `crates/riwork-engine/src/chat/host.rs:1473-1481,1550-1551`; `crates/riwork-engine/src/chat/attachments.rs:66-72` | Read and classify with no lock held. Take the lock only for the deleted check, quota and write. |
| ATT-8 | Low | port (chat) | **Text files starting with "BM" or "P1"–"P7" are refused as images**, because `image::guess_format` matches loose magic bytes. | `crates/riwork-engine/src/chat/attachments.rs:165-168` | Treat only PNG or JPEG magic, or a full decode, as an image; otherwise check UTF-8 text. |
| ATT-9 | Low | live (6c) | **Terminal clipboard PNGs live as long as the component, not the shell.** Detach, a project switch, a window close or a restart deletes a path that the agent may still read. GPUI wrote nothing to disk for a lone picture. | `crates/riwork-nsh-terminal/src/images.rs:43-52,88-92`; `crates/riwork-nsh-terminal/src/macos.rs:182,189`; parity TERM-039 `docs/nsh-parity.md:746` | Tie the lifetime to the shell, with a TTL, or document it. |
| ATT-10 | Low | contract (6c) | **`Files.*` containment roots come from JS** (`listDirectory(root, path)`). With a chosen root, any file is readable. `openEditor` resolves its root from the project id instead. | `crates/riwork-nsh-native/src/files.rs:157-165,224-234`; `crates/riwork-engine/src/file_preview.rs:349-355` | Take `{projectId, worktreeId?}` and resolve the root in Rust. |
| ATT-11 | Low | contract (chat) | **`Chat.create` takes its cwd from JS**, checked only to be an absolute directory. A TSX bug could start an agent in `$HOME` or `/`. | `crates/riwork-nsh-native/src/chat/mod.rs:126-134`; `crates/riwork-engine/src/chat/host.rs:1901-1906` | Derive the cwd from the project or worktree, or validate containment. |
| ATT-12 | Low | doc (6b §1.7, 6c §3.4) | **Stale paste references.** Contracts cite `sessions.rs:1853,1400,1860,1879` (now 1971, 1518, 1978, 1997). The drop path uses `manager.paste` with `terminal_drop::path_input`, not `paste_files`. TERM-214 cites 2097-2098 (now 2216). | `docs/nsh-contracts.md:301,713`; `crates/riwork-nsh-terminal/src/macos.rs:465-485`; `docs/nsh-parity.md:973` | Refresh the references and name the engine path drops use. |
| ATT-13 | Info | port (chat, 6b) | Carry into the ports: (a) remote tabs must never receive local paths or temp images (`macos.rs:429,549-553`); (b) transcript image URLs are fetched with redirects and no private-address filter in GPUI (`crates/riwork-desktop/src/chat_view/media.rs:854-875`), so keep the 10 s / 8 MiB caps and consider blocking loopback; (c) removing a chip never deletes its staged copy (by design); (d) NSH `Chat.delete` does not call `drafts::forget`, only an open handle seeing Deleted does (`chat/mod.rs:155-165,273`); (e) upload-inbox removal has a check-then-remove race open to the same user only (`upload_inbox.rs:22-29`). | as listed | (d) belongs to chat: forget the draft after a successful delete. |

Checked and sound:
- **Source reads.** Open with `O_NOFOLLOW|O_NONBLOCK`, then fstat and a cap+1 read on the
  same descriptor (`attachments.rs:66-88`).
- **Staging.** UUID 0700 directories, 0600 `create_new` files, quota under the lock, deleted
  chats refused (`attachments.rs:241-304`).
- **Send-time re-validation.** Exact path, metadata, re-read, fingerprint and reclassify
  (`attachments.rs:306-367`).
- **Draft store.** UUID names, symlinks skipped, capped, 0600 atomic writes, metadata-only
  recovery (`drafts.rs:184-216,653-737`).
- **Upload inbox.** Canonical UUIDs, removed on shell close and on chat delete
  (`upload_inbox.rs:15-36`).
- **Drop escaping and paste delivery.** Shell-escaped, whole batch refused on any control
  character (`terminal_drop.rs:7-49,149-167`); delivered through tmux stdin under the per-shell
  input lock (`session_input.rs:73-131`).
- **Terminal input gate and PNG staging.** Input requires a live, visible, local, unfrozen pane
  (`macos.rs:422-439`); PNG staging checks magic and caps the size (`images.rs:12-92`).
- **Remote payloads.** Attachments are stripped (`remote_payload.rs:58-72`).

## Dictation teardown as it exists from Phase 4 (chat)

What exists in the NSH build. Phase 4a left dictation as a seam:

- **No dictation code runs.** No `Dictation` module is registered (`crates/riwork-nsh-native/src/lib.rs:37-54`),
  no TSX handler exists, and no NSH code starts `riwork-speech`. The GPUI engine
  (`crates/riwork-desktop/src/dictation.rs`) is declared only by the uncompiled `main.rs`.
- **The shortcut is inert.** `toggleDictation` is a deferred command (`apps/riwork/keyboard.ts:5`),
  and `commandAllowed` keeps it inert (`keyboard.ts:75-78`). Its chord `ctrl+alt+d`
  (`keyboard.ts:22`) is bound window-wide to a no-op that only closes the tab-strip menu
  (`apps/riwork/commands.ts:103-106`).
- **The helper is still shipped.** `riwork-speech` is built (`crates/riwork-desktop/build.rs:1-22,114-133`),
  copied into the bundle and signed (`scripts/bundle-macos.sh:83-94,150-152`). The bundle declares
  `NSMicrophoneUsageDescription` and `NSSpeechRecognitionUsageDescription` (`scripts/bundle-macos.sh:127-128`).
- **The setting survives NSH edits.** NSH Settings shows no dictation option, but its writes patch
  only the selected fields (`crates/riwork-nsh-native/src/settings.rs:314-332`), so `dictation_mic` is kept.

So no teardown path exists, because nothing starts. The risk lies in how the wiring is added.

| ID | Sev | Status | Finding and failure | References | Fix direction |
|---|---|---|---|---|---|
| DIC-1 | Medium | port (chat; 6c for quit) | **Teardown must not depend on JS.** In NSH every window is its own JS runtime in one process. The helper exits only when its stdin closes (CHATC-148), which in GPUI happened when the view entity dropped. If the native owner is not tied to the chat handle, or to the runtime's lifetime, then closing a window, reloading or crashing a runtime leaves the microphone listening while the app keeps running. CHATC-129 (turning the setting off cancels everywhere) and CHATC-147 (one dictation app-wide; a new start cancels the old one and removes its words) also need a process-wide owner in Rust. | parity CHATC-128, -129, -147, -148 `docs/nsh-parity.md:1806-1807,1825-1826`; reference `crates/riwork-desktop/src/dictation.rs:630-707`; `crates/riwork-desktop/src/chat_view/dictate.rs:61-75,106-109`; Phase 4a handle teardown `crates/riwork-nsh-native/src/chat/mod.rs:645-680` | One `Dictation` owner in `riwork/native`. Bind each session to a `ChatHandle` (cancel in `close()` and `Drop`) or to a subscription dropped with the runtime. Subscribe to `dictation_mic`. Cancel at the host's quit gate before exit. Start the helper only while the setting is on, so macOS asks for the microphone only then. Add a bridge test that drops the handle without `close()` and checks the helper exits. |
| DIC-2 | Low | live, unverified (6c, chat) | **The deferred chat chords are bound window-wide** (`ctrl+alt+d`, `mod+.`, `mod+g`, `mod+shift+v`, …). CHATC-130 says that with the mic hidden (the default), ⌃⌥D propagates as if unbound. The source doesn't settle whether an NSH keymap binding wins over a first-responder `riwork.terminal`. If it does, these chords never reach a focused terminal program. Headless tests show only that the chords are inert. | `apps/riwork/commands.ts:103-106`; `apps/riwork/keyboard.ts:20-31`; `apps/riwork/tests/Keyboard.test.tsx:71-88`; parity CHATC-130 `docs/nsh-parity.md:1808`; `docs/nsh-contracts.md:735` | Host test: with a terminal focused, ⌃⌥D, ⌘G and ⌘⇧V reach the PTY. If not, scope the deferred bindings to chat views. |
| DIC-3 | Info | live (chat, 6c packaging) | **The signed speech helper ships, with its usage strings, though nothing uses it.** Nothing starts it, so there is no runtime risk. | `crates/riwork-desktop/build.rs:114-133`; `scripts/bundle-macos.sh:83-94,127-128,150-152` | Decide whether to keep shipping it until DIC-1 lands. |
