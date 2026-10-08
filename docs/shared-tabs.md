# Shared project tabs: desktop and iPhone contract

The desktop owns one project list in `RIWORK_HOME/project-tabs/PROJECT_UUID.json`, protected by a sibling file lock and atomic replacement. Shared state is membership, order, pin, canonical title and visibility. Selection, pane placement and the close preference are device-local. Layout snapshots never overwrite an existing shared list. Metadata-only status refreshes do not increment the durable revision. A reply with the same revision may have fresher status.

## Relay requests

Gate all tab operations and tab UI on `desktopFeatures.tabs`. Older desktops retain the existing inventory adapter.

- `tabs.list`: `{"project_id":"PROJECT_UUID"}`.
- `tabs.update`: `{"project_id":"PROJECT_UUID","update":{"action":"hide","key":"chat:SESSION_UUID"}}`.
- `tabs.open`: `{"project_id":"PROJECT_UUID","key":"shell:SESSION_UUID"}`. Equivalent to the `unhide` update; it does not start a provider or mutate a device's selection.

Updates are `pin`, `unpin`, `hide`, `unhide`, `move`, `rename`. Every update has `key`; move additionally accepts `before` (key or null/end), and rename requires `title` (empty clears the override). Move targets must be in the same sibling and pin group. Only roots may be pinned. Pin unhides; a pinned tab must be unpinned before Hide. The project orchestrator starts pinned. UUIDs must be canonical lowercase UUIDs. Unknown fields are rejected. Titles allow at most 200 Unicode scalars and reject C0/C1 and bidi direction/override/isolate controls, while permitting ZWJ emoji and soft hyphens.

Move request examples (method `tabs.update`):

```json
{"project_id":"PROJECT_UUID","update":{"action":"move","key":"chat:SESSION_UUID","before":"chat:OTHER_SESSION_UUID"}}
```

Use `"before":null` to append to that sibling/pin group. Explicit open/unhide is either `tabs.open` with the shape above or `tabs.update` with `{"action":"unhide","key":"shell:SESSION_UUID"}`. Both return the full authoritative list; neither starts a process. `TabUpdate.move(_:before:)` and `RemoteModel.moveTab(_:before:)` use the same nullable qualified key.

All three operations return the same authoritative shape:

```json
{
  "revision": 12,
  "entries": [{
    "key": "chat:11111111-1111-4111-8111-111111111111",
    "kind": "chat",
    "title": "User chat",
    "status": "waiting",
    "pinned": false,
    "hidden": false,
    "worker": false,
    "order": 1,
    "parent": null,
    "children": [{
      "key": "shell:22222222-2222-4222-8222-222222222222",
      "kind": "shell",
      "title": "Shell · main",
      "status": "working",
      "pinned": false,
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

The wire nests each child once under its parent, with at most eight parent edges. `allEntries` flattens this tree. **Draw `visible`, which includes explicitly opened children**, sorted by shared order. Hide does not implicitly hide descendants. Hidden parents remain discoverable in the picker. `child_count` counts direct children, including hidden ones. Extra internal fields may be present and should be ignored. Kinds are `chat`/`shell`; status is `working`/`waiting`/`error`/`done`/`stopped`. Unknown kinds/status decode as `.unknown`; ignore unsupported kinds. Stopped shells are not visible/openable. Stopped chat history remains available.

## What is a worker?

A session with a same-project parent is a worker. `riwork chat new` and `riwork shell create` inherit `RIWORK_CHAT_ID`, otherwise `RIWORK_SHELL_ID`; `--no-parent` opts out. Plain-terminal commands without a parent and sessions created by the user's + action are roots and visible. A harness shell from older/background inventory with no explicit `user_opened` provenance is also a worker. Project orchestrators are roots, not workers. Provider-internal subagents without a RiWork session identity are not fabricated as tabs.

Workers are registered in membership **hidden by default**. `worker` persists the close policy even if the parent exits and the entry becomes a root. `parent` is a qualified key or null. Missing/deleted parents and dead shell parents promote children to roots, retaining worker policy and their existing visibility. Hiding a parent does not promote its children. Opening any hidden entry deliberately unhides it on both devices. New metadata polling does not re-hide an opened worker or resurrect a hidden tab. Hidden stopped leaf chat history is bounded at 200 with retirement records preventing resurrection; visible history and hidden parents are retained.

## iOS model API for ios-chat-chrome

`ios/Core/SharedTabs.swift` supplies `SharedTab`, `SharedTabsReply`, `TabUpdate`, `TabCloseBehavior`, validation and transport `listTabs(projectID:)`, `updateTabs(projectID:update:)`, `openTab(projectID:key:)`.

`RemoteModel` exposes:

- `sharedTabs: SharedTabsReply?`, `hiddenTabs: [SharedTab]` (hidden, openable chats/shells), `tabCloseBehavior`.
- `listTabs()` and `updateTabs(_:)`, both async throwing and capability gated.
- `openTab(_ key: String)`: shared Unhide followed by device-local chat/shell selection. No process creation.
- `closeTab(_ key: String, choice: TabCloseBehavior? = nil) async throws -> Bool`: false means Ask requires a sheet, with **no mutation**. A supplied choice is the sheet result. Workers force Detach regardless of the supplied choice/preference. True means the requested operation completed. Failures throw; UI displays them.
- `pinTab`, `unpinTab`, `hideTab`, `unhideTab`, `moveTab(_:before:)`, `renameTab(_:title:)` wrappers.
- Legacy `openChildTab`/`closeChildView` wrappers remain; new chrome should use `openTab`/`closeTab`.

Use `sharedTabs.visible` for chrome and `hiddenTabs` for “Open a worker/shell”. After a mutation install its reply, without an optimistic hide or automatic retry of an uncertain result. Connection/project generations discard stale replies, and smaller revisions are ignored. Same-revision status is accepted. Creation fetches shared membership after inventory. Connected foreground session refreshes run every four seconds, including focus mode. Mac refreshes every two seconds. Phone connection does not independently seed membership.

## Close setting and presentation

The wording on both platforms is **When closing a tab: Ask / Detach / Exit**. The device-local key is `tab_close_behavior`, serialized as `ask` (default), `detach`, `exit`; iOS stores it in UserDefaults and Mac in settings.json.

- Worker: Hide immediately, no prompt, process continues.
- User-opened chat/shell: Ask shows Detach and Exit plus Cancel. Detach hides and keeps the process; Exit hides, then sends `chat.stop` or destructive `shell.close`. Chat history stays. A stop failure is surfaced after the hide, and must not be silently retried.
- Mac uses a Kit/Base dialog with focus trap and Escape. iOS uses a confirmation sheet. Non-Ask preferences skip the prompt.

The existing destructive **Close terminal** action and Stop agent action remain separate session controls. `shell.close` keeps its destructive semantics and is never used for Detach. Remote Mac tabs (⇄ HOST) keep their existing behavior. Global orchestrators remain device-local views outside project membership and remain openable through their existing action.

## CLI

`riwork tabs list --project UUID`, `riwork tabs update --project UUID --update-json JSON`, `riwork tabs open --project UUID --key KIND:UUID` return the authoritative wire reply. `riwork chat open UUID` unhides an existing chat and queues a local desktop open request without starting the host. Storage/policy failures are surfaced as `not_found`, `invalid_request`, or `cli_error` over the relay.
