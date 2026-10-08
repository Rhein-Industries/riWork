# Shared project tabs: desktop and iPhone contract

The desktop owns one project list in `RIWORK_HOME/project-tabs/PROJECT_UUID.json`, protected by a sibling file lock and atomic replacement. Shared state is membership, order, canonical title and visibility. There are no pins: stores written while pins existed keep their order and lose the `pinned` field on their next write. Selection, pane placement and the close preference are device-local. Layout snapshots never overwrite an existing shared list. Metadata-only status refreshes do not increment the durable revision. A reply with the same revision may have fresher status. Each store has a persistent UUID `epoch`; resetting/deleting its file creates a new epoch so clients can accept a lower revision. Older stores acquire an epoch on their next successful transaction.

## Relay requests

Gate all tab operations and tab UI on `desktopFeatures.tabs`. Older desktops retain the existing inventory adapter.

- `tabs.list`: `{"project_id":"PROJECT_UUID"}`.
- `tabs.update`: `{"project_id":"PROJECT_UUID","update":{"action":"hide","key":"chat:SESSION_UUID"}}`.
- `tabs.open`: `{"project_id":"PROJECT_UUID","key":"shell:SESSION_UUID"}`. Equivalent to the `unhide` update; it does not start a provider or mutate a device's selection.

Updates are `hide`, `unhide`, `move`, `rename`; `pin` and `unpin` (from older phones) are rejected as `invalid_request`. Every update has `key`; move additionally accepts `before` (key or null/end), and rename requires `title` (empty clears the override). Move takes any tab before any other (or to the end); a worker may be moved beside tabs that are not its siblings and keeps its parent. (Before this, Move required the same parent and pin group; clients that still send only siblings stay valid.) Any tab can be hidden. The project orchestrator is an ordinary tab: it is first only because it is created first, moves like any other and closes with the same Detach/Exit prompt as a user-opened chat. UUIDs must be canonical lowercase UUIDs. Unknown fields are rejected. Titles allow at most 200 Unicode scalars and reject C0/C1 and bidi direction/override/isolate controls, while permitting ZWJ emoji and soft hyphens.

Move request examples (method `tabs.update`):

```json
{"project_id":"PROJECT_UUID","update":{"action":"move","key":"chat:SESSION_UUID","before":"chat:OTHER_SESSION_UUID"}}
```

Use `"before":null` to move to the end. Explicit open/unhide is either `tabs.open` with the shape above or `tabs.update` with `{"action":"unhide","key":"shell:SESSION_UUID"}`. Both return the full authoritative list; neither starts a process. `TabUpdate.move(_:before:)` and `RemoteModel.moveTab(_:before:)` use the same nullable qualified key.

All three operations return the same authoritative shape:

```json
{
  "epoch": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "revision": 12,
  "entries": [{
    "key": "chat:11111111-1111-4111-8111-111111111111",
    "kind": "chat",
    "title": "User chat",
    "status": "waiting",
    "hidden": false,
    "worker": false,
    "order": 1,
    "parent": null,
    "children": [{
      "key": "shell:22222222-2222-4222-8222-222222222222",
      "kind": "shell",
      "title": "Shell · main",
      "status": "working",
      "hidden": true,
      "worker": true,
      "order": 2,
      "parent": "chat:11111111-1111-4111-8111-111111111111",
      "children": [],
      "child_count": 0
    }],
    "child_count": 1
  }]
}
```

The wire nests each child once under its parent, with at most eight parent edges. `allEntries` flattens this tree. **Draw `visible`, which includes explicitly opened children**, sorted by shared order. Hide does not implicitly hide descendants. Hidden parents remain discoverable in the picker. `child_count` counts direct children, including hidden ones. Extra internal fields may be present and should be ignored. Kinds are `chat`/`shell`; status is `working`/`waiting`/`error`/`done`/`idle`/`stopped`. A shell's status: `stopped` when its tmux session is gone; `working` while its agent works, `waiting` while it waits on an approval; `done` once an agent's run has ended; `error` for a failed program (not reported yet: RiWork records no exit status for shell programs); otherwise `idle` (a live shell at its prompt, or running a program RiWork does not follow). `idle` is new: clients that predate it decode it as `.unknown`, and iOS should add it to `SharedTab.Status`. Unknown kinds/status decode as `.unknown`; ignore unsupported kinds. Stopped shells are not visible/openable. Stopped chat history remains available.

## Tab titles

A tab's canonical title is set once and changes only when a higher source arrives. From highest:

1. **Rename** — `rename` in the shared list (shown over everything, empty clears it), or an explicit chat name (`chat new --title`, `ChatInfo.user_title`). A newer explicit name replaces an older one.
2. **Orchestrator name** — the project orchestrator is always **Project orchestrator**, the global one **Global orchestrator**. No message, provider title or the `P·ORCH`/`G·ORCH` label it was created with renames them; only a rename does.
3. **Provider title** — the provider's own name for the session, stored in `ChatInfo.provider_title` when it arrives: Codex's `thread.name` on `thread/start`/`thread/resume` and `thread/name/updated` (Codex names a thread only when it is renamed there); Claude's `custom-title`, else `ai-title`, read from its transcript (`<CLAUDE_CONFIG_DIR or ~/.claude>/projects/*/<session>.jsonl`) after each turn, since stream-json carries no title. It replaces a first-message title, never a rename.
4. **First message** — the first accepted message of a chat that has no title yet, as its first sentence (or first line), cut at a word boundary with an ellipsis past 60 characters. It is stored once in `ChatInfo.first_user_message`; a resume, host restart, compaction, reconnect or re-read of the log never derives it again, and a later "first" message never replaces it.
5. **Kind** — **Claude chat** / **Codex chat**.

Shells are titled `program · branch` (`VIM · file` for an editor), also set once: a later directory or program change does not retitle them; only a rename does. Shell names that collide are numbered (`zsh · main · 2`).

Migration: an orchestrator whose stored title came from its first message (the old 40-character cut) gets its fixed name back; every other stored title is kept.

## What is a worker?

A chat with a same-project parent is a worker. A shell with a parent, or without explicit `user_opened` provenance, is a worker; this includes unsolicited harness shells. Project orchestrators are roots, not workers. Provider-internal subagents without a RiWork session identity are not fabricated as tabs.

`riwork chat new` and `riwork shell create` inherit `RIWORK_CHAT_ID`, otherwise `RIWORK_SHELL_ID`; `--no-parent` opts out of the parent relationship. GUI shell +/new-terminal/new-harness and GUI handoff use `SessionManager.for_user()`: no inherited parent and `user_opened=true`. CLI `handoff --to shell` follows the same caller rule as `shell create`: it preserves a same-project `RIWORK_CHAT_ID`/`RIWORK_SHELL_ID` parent instead of forcing a user-opened root. Plain-terminal `shell create` is user-opened only when none of `RIWORK_CHAT_ID`, `RIWORK_SHELL_ID`, or `RIWORK_ORCHESTRATOR_SCOPE` is nonempty. Caller context still makes a shell a worker with `--no-parent`. Automation can use `shell create --background` or `SessionManager.for_automation()` even without a parent. Phone user shell creation removes these caller markers before invoking the CLI. All non-user shell creation records `user_opened=false`.

On first migration, tabs in saved `layouts.json` panes start visible; other existing chats/shells start hidden, except the project orchestrator, which starts visible unless it was explicitly detached in the saved layout. A parentless legacy shell in a saved pane counts as user-opened, even if its old registry lacks `user_opened`. A parent-bearing session remains a worker and starts hidden regardless of saved placement. This migration provenance survives later inventory polls. Subsequent user-created sessions start visible normally.

Workers are registered in membership **hidden by default**. `worker` persists the close policy even if the parent exits and the entry becomes a root. `parent` is a qualified key or null. Missing/deleted parents and dead shell parents promote children to roots, retaining worker policy and their existing visibility. Hiding a parent does not promote its children. Opening any hidden entry deliberately unhides it on both devices. New metadata polling does not re-hide an opened worker or resurrect a hidden tab. Hidden stopped leaf chat history is bounded at 200 with retirement records preventing resurrection; visible history and hidden parents are retained. Explicit deletion tombstones are bounded at 1,024 and evict the oldest insertion first. A missing/unreadable per-chat `info.json` is skipped with a warning; it does not invalidate other entries or prune an existing entry while its directory remains. A failure to read the whole inventory/store is surfaced without treating it as an empty list.

## iOS model API for ios-chat-chrome

`ios/Core/SharedTabs.swift` supplies `SharedTab`, `SharedTabsReply`, `TabUpdate`, `TabCloseBehavior`, validation and transport `listTabs(projectID:)`, `updateTabs(projectID:update:)`, `openTab(projectID:key:)`.

`RemoteModel` exposes:

- `sharedTabs: SharedTabsReply?`, `tabCloseBehavior`. The picker's rows are `SharedTabStrip.openable(sharedTabs)` (hidden, openable chats/shells, workers first).
- `listTabs()` and `updateTabs(_:)`, both async throwing and capability gated.
- `openTab(_ key: String)`: shared Unhide followed by device-local chat/shell selection. No process creation.
- `closeTab(_ key: String, choice: TabCloseBehavior? = nil) async throws -> Bool`: false means Ask requires a sheet, with **no mutation**. A supplied choice is the sheet result. Workers force Detach regardless of the supplied choice/preference. True means the requested operation completed. Failures throw; UI displays them.
- `hideTab`, `unhideTab`, `moveTab(_:before:)`, `renameTab(_:title:)` wrappers.

Use `sharedTabs.visible` for chrome and `SharedTabStrip.openable` for “Open shell/worker…”. After a mutation install its reply, without an optimistic hide or automatic retry of an uncertain result. Connection/project generations discard stale replies. `SharedTabsReply.supersedes(_:)` accepts a changed `epoch` even at a lower revision; within the same epoch smaller revisions are ignored and same-revision status is accepted. `epoch` is optional for older hosts, which retain the revision-only comparison when both replies omit it. Creation fetches shared membership after inventory. Connected foreground session refreshes run every four seconds, including focus mode. Mac refreshes every two seconds. Phone connection does not independently seed membership.

## Close setting and presentation

The wording on both platforms is **When closing a tab: Ask / Detach / Exit**. The device-local key is `tab_close_behavior`, serialized as `ask` (default), `detach`, `exit`; iOS stores it in UserDefaults and Mac in settings.json.

- Worker: Hide immediately, no prompt, process continues.
- User-opened chat/shell, and the project orchestrator: Ask shows Detach and Exit plus Cancel. Detach hides and keeps the process; Exit hides, then sends `chat.stop` or destructive `shell.close`. Chat history stays. A stop failure is surfaced after the hide, and must not be silently retried.
- A failed shared Hide aborts the entire close, displays an error, and sends no Stop/Exit. Mac keeps its last good shared list. If the store could not load (`shared_tabs == None`), Detach may hide only the local view using device-local layout dismissal state; it sends no process command and makes no shared visibility change. Exit remains refused with an error until the store is available. Once shared state recovers, its authoritative visibility can reopen a locally detached view. If an entry is missing, Mac checks registry/log parent and user-open provenance; unknown provenance defaults to Detach.
- Mac uses a Kit/Base dialog with focus trap and Escape. iOS uses a confirmation sheet. Non-Ask preferences skip the prompt.

The existing destructive **Close terminal** action and Stop agent action remain separate session controls. `shell.close` keeps its destructive semantics and is never used for Detach. Remote Mac tabs (⇄ HOST) keep their existing behavior. Global orchestrators remain device-local views outside project membership and remain openable through their existing action.

## Desktop strip

A local project's pane bar is the one tab strip; there is no separate shared row. It is drawn from the shared entries: built-in panels (Shells, Files, Settings, …) as a leading run of flat symbol cells, then the session tabs in shared order, as flat square cells parted by hairlines, the selected one filled. Each session tab is a Kit button with the tab role, a status dot (working in the theme's working accent, waiting gold, error red, done muted, idle a small muted point, stopped an empty ring, as on the phone), the canonical title and a close mark shown on the selected tab and under the pointer. VoiceOver reads title and status. Middle click closes. ＋ offers New terminal, New Claude chat, New Codex chat and Open a worker or shell… (hidden chats/shells with kind, status and parent). When the tabs do not fit they scroll sideways and an All tabs button lists every session tab; the strip never wraps. ⌘1–8 select session tabs by position, ⌘9 the last; Ctrl+Tab and ⌘⇧]/[ walk the strip's order.

Right-click a tab, or focus it and press Shift+F10/the Menu key, for Rename…, Move left/right and Close. Close uses the Ask/Detach/Exit policy above; the Ask dialog freezes the terminals like the other modals so it draws above them. Move, and dragging a tab within its strip, publishes Move: the menu's Move left/right step past the adjacent tab of the same parent, a drag goes anywhere in the strip; dragging a tab into another pane changes local placement only. Each split pane draws its own strip from the tabs placed in it. Remote (⇄ HOST) projects keep the previous pane bar. The strip uses the standard macOS window-controls content inset only while window controls are visible; fullscreen has no inset.

## CLI

`riwork tabs list --project UUID`, `riwork tabs update --project UUID --update-json JSON`, `riwork tabs open --project UUID --key KIND:UUID` return the authoritative wire reply. `riwork chat open UUID` unhides an existing chat and queues a local desktop open request without starting the host. CLI and relay report malformed requests/keys/titles/policy violations as `invalid_request`, unknown projects/sessions as `not_found`, and storage/execution failures as `cli_error`.
