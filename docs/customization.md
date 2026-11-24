# Customizing Somtus

Somtus's project windows can use a personal interface built from JSON layouts and
native TSX add-ins. Saved edits apply while the app runs. The normal interface is
the default; your projects, panes, tabs, terminal processes and chats remain owned
by the existing workspace runtime.

Open **Settings → Windows → Open customization folder** to create the starter
files and open the folder. Select an interface in the same settings section.
`somtus customization init` also creates the files without overwriting existing
ones. `somtus customization select sidebar` selects the supplied command sidebar.

**Minimalist** is a bundled workspace mod. Enable it under **Settings → Mods**
to keep Projects, Files, CLI terminals and Settings in your project windows.
File previews open inside Files. Disable the mod to return to your previous
interface and see the hidden tabs again. Running sessions continue to run.

Files live under `RIWORK_HOME/ui` (`~/.local/share/riwork/ui` by default, retaining
Somtus's existing data directory):

| File | Purpose |
| --- | --- |
| `config.json` | Selected interface: `{"version":1,"layout":"sidebar"}` |
| `layouts/<name>.json` | Personal interface composition |
| `mods.json` | Active project workspace mod |
| `keys.json` | Window and application shortcut overrides |
| `README.md` | Local guide for you and your coding agent |

These are user files, separate from the app bundle. An app update does not
replace them. To share a setup, copy its layout, key overrides, and referenced
add-in sources. The receiving user chooses the layout and trusts any add-in
permissions. The files contain no terminal sessions or project layout state.

## Ship and share mods

A workspace mod contributes code to the project workspace. Minimalist ships
with its own [manifest](../apps/riwork/mods/minimalist/mod.json) and
[TSX module](../apps/riwork/mods/minimalist/App.tsx). To ship another reviewed
workspace mod, register its module in the [workspace mod registry](../apps/riwork/mods/index.ts)
and its manifest in the [native catalog](../crates/riwork-core/src/customization.rs).
The app build includes both. Mod authors can use TSX with the host's existing
pane and command APIs. Extending the native APIs requires a rebuild of the app.

Project add-ins can travel with their repository under `.ontack/addins`. Copy
personal add-ins to `RIWORK_HOME/addins`. Somtus can load prebuilt add-in bundles,
but `somtus addin pack`, `install`, and `catalog` are still unavailable. There is
no automatic over-the-air distribution for mods today.

## Compose the interface

```json
{
  "version": 1,
  "name": "My workspace",
  "root": {
    "type": "row",
    "id": "frame",
    "children": [
      {
        "type": "commands",
        "id": "navigation",
        "size": 200,
        "items": [
          { "command": "openProjects", "label": "Projects" },
          { "command": "openFiles", "label": "Files" },
          { "command": "newChat", "label": "New chat" },
          { "command": "openCustomization", "label": "Customize" }
        ]
      },
      { "type": "workspace", "id": "workspace" }
    ]
  },
  "statusBar": true,
  "tabBar": "default"
}
```

Each node has a unique `id`. A `row` lays out children horizontally; a `column`
lays them out vertically. `size` requests pixels along its parent's axis; omitted
sizes share the remaining space. Fixed blocks shrink when the window is too
small. `spacer` makes an empty block. Exactly one `workspace` leaf is required.
The workspace keeps the same native parent while its rectangle changes, so
rearranging this tree does not recreate terminal or chat views.

`commands` blocks call the existing window dispatcher. A `mod` block embeds an
add-in view:

```json
{
  "type": "mod",
  "id": "navigator",
  "addin": "id:example.workspace-navigator",
  "view": "main",
  "args": {},
  "size": 300
}
```

`id:<manifest.id>` is portable between personal and project installations. An
explicit key such as `personal/example.workspace-navigator` selects that scope.
Trust checks still apply. Missing, untrusted or failed-to-build mods show an explanation
in their block. Existing source add-ins rebuild live when saved; a compilation
failure leaves their previous working version visible.

To replace a built-in panel, add a `panels` map to the document:

```json
{
  "projects": { "addin": "id:example.workspace-navigator", "view": "main" }
}
```

That is the value of the `panels` field, not a complete layout. Other panels
continue working normally. `statusBar: false` hides the built-in status bar.
`tabBar: "hidden"` hides the built-in pane tab strips, allowing a custom navigator
to own tab selection. Pane shortcuts and session state remain available. Custom
interfaces keep a native window drag region and visible Edit/Reset controls.

## Give a mod workspace access

Add `workspace.read` and/or `workspace.control` to its manifest's `permissions`.
An existing add-in requests renewed trust when its permissions change.

```ts
import { workspace } from "./addin-api";

const state = await workspace.get();
const stop = await workspace.subscribe((next) => {
  // next.projectId, next.activePane, next.panes[].tabs
});
await workspace.command("selectTab", { id: state.panes[0].tabs[0].id });
await workspace.command("openFiles");
stop();
```

Use the generated `addin-api.ts` for the exact typed command list. Context is
limited to project and selection identifiers, panes, tab titles/kinds/statuses;
it does not expose transcript text, file contents, credentials or raw native
modules. Commands act in the mod's owning window. Context deliveries update a
running mod without rebuilding it. Command promises acknowledge dispatch, not
completion of the resulting user operation. Subscribe cleanup belongs in the
mod's normal lifecycle cleanup.

The complete example in `examples/addins/workspace-navigator` lists live panes
and tabs and offers navigation/action buttons. Copy its directory under
`RIWORK_HOME/addins`, copy `examples/customization/navigator.json` under
`RIWORK_HOME/ui/layouts`, then select `navigator`. Review and trust the add-in in
the app.

## Change shortcuts

`keys.json` maps supported command names to chords; `null` unbinds a command:

```json
{
  "version": 1,
  "bindings": {
    "nextTab": "mod+alt+j",
    "previousTab": "mod+alt+k",
    "splitDown": null
  }
}
```

`mod` is Command on macOS. The old chord stops firing and native menu
accelerators update with the same map. Unspecified commands keep their defaults.
Conflicting chords, unsupported commands and reserved text/chat shortcuts are
rejected. Context-specific chat editing shortcuts remain owned by the chat.

## Recover from an edit

Invalid configuration retains the last accepted configuration in the running
process and displays the error. At a fresh launch, each invalid part falls back
to its defaults. Fix the file and save to resume live updates.

**Control–Option–Shift–Escape** is handled by the native host and cannot be
rebound. It restores the built-in interface and archives the custom shortcut
file. **Reset interface and shortcuts** in Settings and
`somtus customization reset` do the same. Reset also disables the active
workspace mod. Your custom layout files, add-ins, project pane layouts, sessions
and chats are retained.

The declarative composition currently applies to project windows. The Global
dashboard/chat keeps its built-in interface and honors its supported shortcut
overrides. The native window manager, terminal renderer, and context-specific
chat editing behavior remain application-owned.
