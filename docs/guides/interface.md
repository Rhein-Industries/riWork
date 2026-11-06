# Workspace controls

[Documentation index](../README.md) · [Project overview](../../README.md)

Repository paths and commands below are relative to the checkout root.

## UI controls

Somtus has no dialogs: nothing covers the window, freezes the terminals or waits for an answer. New chat and Hand off open a tab of their own; closing a tab closes it at once and its notice offers **Undo** (and, with **Offer to end** in Settings, ending the session); a tab is renamed in the tab itself (**Rename…** or a double click); **Open file in Vim…** is a bar at the bottom of the window that suggests the folder's files as you type; a panel's forms (new folder, project or task) unfold under its toolbar and ending a shell is confirmed in its row; remote hosts are added and paired in the Remote section of Settings; a dashboard decision is reviewed in its row; and a chat's file review, code reader and image preview open beside its transcript, with **Copy to:** a terminal as a row under the reply. Escape folds what is open when the focus is in it.

| Action | Shortcut |
| --- | --- |
| New shell tab | Cmd+T |
| Open Settings | Cmd+, |
| Create project | Cmd+N |
| New project window | Cmd+Shift+N |
| Split right / down | Cmd+D / Cmd+Shift+D |
| Close tab / pane (shell stays alive; refused in a locked pane) | Cmd+W / Cmd+Shift+W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Search current panel (opens Projects from a shell tab) | Cmd+F |
| Find in a chat (its transcript or message box focused). While its bar is open, Return / Shift+Return or Cmd+G / Cmd+Shift+G go to the next / previous match (so Cmd+Shift+G does not open Grok then), and Esc closes it | Cmd+F |
| Open / select Projects panel | Cmd+B |
| Open / select Files / Preview panel | Cmd+Shift+E / Cmd+Shift+P |
| Global window: Chat / Dashboard | Cmd+Shift+O / Cmd+Option+G |
| Open project orchestrator tab | Cmd+Alt+O |
| Project Settings (Codex account) | Cmd+Option+A |
| Focus current tab / restore workspace | Cmd+Shift+F |
| Gather the tabs of unlocked panes into the main pane | Cmd+Shift+M |
| Apply the default layout (locked navigation pane on the left, everything else in the main pane) | Cmd+Option+L |
| Open Codex / Claude / Grok / Muse | Cmd+Shift+C / Cmd+Shift+L / Cmd+Shift+G / Cmd+Shift+U |
| New chat (any model, Codex's or Claude's) | Cmd+Option+Shift+C |
| Interrupt the turn of a chat | Cmd+. |
| Bigger / smaller / actual-size app text (outside a terminal) | Cmd+= or Cmd++ / Cmd+- / Cmd+0 |
| Open a URL or file in a terminal / open a file there in Vim | Cmd+click / Cmd+Shift+click |

The text-size keys resize Somtus's own text only while focus is outside a terminal; in a focused terminal they reach Ghostty, which zooms that terminal's font as before. **View → Bigger Text / Smaller Text / Actual Size** in the menu bar always resize the app text. Focusing a terminal, by click or by opening a tab, ends a Cmd+F search so typing reaches the shell only.

Panel tabs start in a left pane. Every pane's tab strip stays at the top, and every panel and shell tab can move, close (unless its pane is locked), or share a split:

- Drag a tab along a strip to reorder it, or onto another strip or pane center to move it into that pane.
- Drop on a pane edge to create a split. Drop within 12 px of a window edge to dock across the entire workspace.
- Drag the 5 px split dividers to resize panes.
- Drag empty strip space on a strip touching the window's top edge to move the window; double-click that area to zoom. A blank drag area beside the pane controls stays reachable when tabs overflow.
- The **…** actions menu groups **New Tab** launches, a **Tab** section with **Hand off…** while a Codex, Claude, Grok or Muse terminal tab is selected (see [Handoff](handoff.md#handoff)), **Views** for panel tabs, and **Pane** actions for splitting or closing the pane, making it the main pane (see **Main pane** under [Projects](projects-and-tasks.md#projects-worktrees-and-tasks)) and gathering tabs into that one. The tab's **X** closes only that tab; the labelled **Close pane** action closes its region. Both preserve shell sessions. Closing the active pane focuses the neighbouring pane that takes its space.
- The lock icon keeps a pane across project switches and keeps its tabs open. In a locked pane the tabs show no **X**, the **Close pane** row is left out of the **…** menu, and Cmd+W and Cmd+Shift+W do nothing but show "Unlock the pane to close its tabs" (or "…to close it"). Unlocking brings all of these back at once. Dragging, reordering, and selecting tabs keep working, tabs can still be dragged out to another pane, and a shell that exits, a project switch, or a restore is not affected. The focus icon or Cmd+Shift+F focuses the active tab in any pane, hiding tab strips and other panes while keeping the configured status bar visible with project context and live information. Narrow panes keep lock and focus actions in the **…** menu. Centered focus caps the content at 1,100 px and leaves 30% of the available area above the status bar clear below it so terminal input sits higher on screen. The top bar has the same height as a normal pane bar and keeps native window controls, **RESTORE**, and a **FILL WINDOW / CENTER FOCUS** toggle visible. Click **RESTORE** or press Cmd+Shift+F again to restore the same split layout and sizes. Ctrl+Tab and Ctrl+Shift+Tab still cycle tabs within the focused pane. Focus mode is temporary and does not rewrite the saved split tree.

The workspace fills the window around a 78 × 28 px native close/minimize/maximize island and the full-width status bar. In macOS full screen, the custom island is hidden and the native controls appear when the menu bar drops down; maximized windows keep the island. The installed `riwork-workspaces` skill teaches Codex the CLI workflow for project tasks, batched worktree assignments, and shell inspection.
