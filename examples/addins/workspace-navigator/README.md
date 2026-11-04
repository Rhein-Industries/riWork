# Workspace navigator

An optional Somtus add-in for the existing workspace. It lists the owning window's
panes and tabs, selects them, and dispatches the same navigation/layout commands
as the built-in controls. It does not replace the workspace model or move tabs
between project windows.

Copy this folder into a project's `.ontack/addins/workspace-navigator` or your
Somtus home's `addins/workspace-navigator`, then open Add-ins and review its trust
request. This example is not installed or trusted automatically. Somtus supplies
`addin-api.ts` when building, so do not copy or edit that generated file.

Use the portable reference `id:example.workspace-navigator`, view `main`, in a
customization slot. The canonical discovery key is `personal/example.workspace-navigator`
or `project-PROJECT_UUID/example.workspace-navigator`. Project add-ins take
precedence over a personal add-in with the same manifest ID. Trust is always
recorded against the canonical discovered key.

`workspace.read` grants metadata only: project/window IDs, pane/tab IDs, titles,
kinds, status, and selected worktree/task IDs. It does not expose project roots, terminal
output, transcripts, drafts, arbitrary native calls, or other windows. `get()`
reads the latest snapshot; `subscribe(listener)` delivers an initial snapshot
and changed snapshots, returning a promise for an unsubscribe function. Updates
keep the embedded add-in engine running.

The example reads initial colors from `info().palette` and listens with
`onPaletteChanged(listener)`. Theme changes update its colors in place without
reloading it; palette events require no permission.

`workspace.control` permits the SDK's explicit command list. Selection commands
take only `{id}`; other commands take no arguments. Successful resolution means
the action was dispatched. The owning workspace checks current membership and
applies its normal locked-pane, dialog, and session rules. This permission is
independent of `workspace.read`; changing either grant requires trust again.
