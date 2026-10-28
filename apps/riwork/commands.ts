import { cleanup, effect, host, keymap, signal } from "nsh";
import type { LayoutSession } from "riwork/native";
import { commandAllowed, DEFERRED_KEYMAP, PANEL_COMMANDS, WINDOW_KEYMAP } from "./keyboard";
import { activePane, drawnTabs, type HostParams, type WorkspaceSnapshot } from "./model";

/** Workspace-owned effects that the dispatcher routes but does not implement. */
export interface CommandHooks {
  /** The close dialog, rename, panel editors and project switches block window commands. */
  blocked: () => boolean;
  /** Create a local shell, optionally splitting the active pane first. */
  newShell: (split?: "right" | "down") => void;
  newSubshell?: () => void;
  /** Consumes an agent shortcut only in a remote project. */
  remoteAgent?: (kind: string) => boolean;
  openVim?: () => void;
  appPreview?: () => void;
  /** Terminal owns Find; panels keep their own search input. */
  find?: (id: string, args?: { query?: string }) => boolean;
  closeTab: () => void;
  closePane: () => void;
  toggleFocus: () => void;
  toggleCentered: () => void;
  tabContextMenu: () => void;
  /** Open a view; Dashboard is routed to the global window. */
  view: (panel: string) => void;
  /** Shared-strip or pane-order numbering, and tab cycling. */
  selectNumbered: (n: number) => void;
  cycle: (direction: 1 | -1) => void;
  /** Dismiss workspace-owned transient UI on Escape. */
  escape: () => void;
  /** A focused native preview owns Escape unless transient workspace UI does. */
  previewOwnsEscape?: () => boolean;
  /** The window's current project, which a panel project switch can change. */
  projectId: () => string | undefined;
  /** ⌘⌥O: the project's orchestrator, found or made as Settings say. */
  projectOrchestrator?: () => void;
  /** The new-tab menu's Global orchestrator: the global window's Chat, started. */
  globalOrchestrator?: () => void;
}

/** Both native menus and key bindings enter this one window-local dispatcher. */
export function workspaceCommands(
  session: LayoutSession,
  snapshot: () => WorkspaceSnapshot,
  action: (run: () => unknown) => void,
  hooks: CommandHooks,
) {
  const stripOpen = signal(false);
  const stripIndex = signal(0);
  const params = host.params as unknown as HostParams;
  function stripItems() {
    const pane = activePane(snapshot());
    return pane ? drawnTabs(pane) : [];
  }
  function toggleStrip() {
    if (hooks.blocked()) return;
    if (stripOpen()) { stripOpen.set(false); return; }
    const selected = activePane(snapshot())?.activeTab;
    stripIndex.set(Math.max(0, stripItems().findIndex((tab) => tab.id === selected)));
    stripOpen.set(true);
  }
  function activateStrip(index = stripIndex()) {
    const tab = stripItems()[index];
    stripOpen.set(false);
    if (tab) action(() => session.selectPanel(tab.id));
  }
  // Menu arrows change only the highlighted row. These bindings disappear on
  // dismissal, restoring the previous key owner before another action runs.
  effect(() => {
    if (!stripOpen()) return;
    const bindings: (() => void)[] = [];
    for (const [key, direction] of [["up", -1], ["left", -1], ["down", 1], ["right", 1]] as const) {
      bindings.push(keymap.bind(key, () => {
        const count = stripItems().length;
        if (count) stripIndex.set((stripIndex() + direction + count) % count);
      }, { repeat: true, when: "always" }));
    }
    for (const key of ["enter", "space"]) bindings.push(keymap.bind(key, () => activateStrip(), { when: "always" }));
    return () => { for (const unbind of bindings) unbind(); };
  });
  function dispatch(id: string, args?: { query?: string }) {
    const agent = ({ openCodex: "codex", openClaude: "claude", openGrok: "grok", openMuse: "muse" } as Record<string, string>)[id];
    if (agent && !hooks.blocked() && hooks.remoteAgent?.(agent)) { stripOpen.set(false); return; }
    if (!hooks.blocked() && ["find", "search", "findNext", "findPrevious", "findClose"].includes(id)
      && hooks.find?.(id === "search" ? "find" : id, args)) { stripOpen.set(false); return; }
    if (!commandAllowed(id, hooks.blocked(), () => stripOpen.set(false))) return;
    const panel = PANEL_COMMANDS[id] ?? (id.startsWith("panel:") ? id.slice(6) : undefined);
    if (panel) { hooks.view(panel); return; }
    if (/^selectTab[1-9]$/.test(id)) hooks.selectNumbered(Number(id.slice(-1)));
    else if (id === "nextTab" || id === "previousTab") hooks.cycle(id === "nextTab" ? 1 : -1);
    else if (id === "gatherTabs" || id === "gather") action(() => session.gather());
    else if (id === "defaultLayout") action(() => session.defaultLayout());
    else if (id === "newTab" || id === "newShell") hooks.newShell();
    else if (id === "newSubshell") hooks.newSubshell?.();
    else if (id === "openVim") hooks.openVim?.();
    else if (id === "appPreview") hooks.appPreview?.();
    else if (id === "splitRight" || id === "splitDown") hooks.newShell(id === "splitDown" ? "down" : "right");
    else if (id === "closePane") hooks.closePane();
    else if (id === "closeTab") hooks.closeTab();
    else if (id === "tabContextMenu") hooks.tabContextMenu();
    else if (id === "toggleFocusMode" || id === "focusMode" || id === "toggleFocus") hooks.toggleFocus();
    else if (id === "focusCentered") hooks.toggleCentered();
    else if (id === "openProjectOrchestrator") hooks.projectOrchestrator?.();
    else if (id === "openGlobalOrchestrator") hooks.globalOrchestrator?.();
    else if (id === "newProjectWindow") {
      void host.call("window.open", { kind: "project", projectId: hooks.projectId() ?? params.projectId }).catch(() => {});
    }
  }
  for (const [key, id] of WINDOW_KEYMAP) cleanup(keymap.bind(key, () => dispatch(id),
    id === "nextTab" || id === "previousTab" ? { when: "always" } : undefined));
  // Panel search owns ⌘F; the remaining deferred chords stay inert.
  for (const key of new Set(DEFERRED_KEYMAP.map(([key]) => key))) {
    const agent = DEFERRED_KEYMAP.find(([chord, id, context]) => chord === key && context === "window" && ["openCodex", "openClaude", "openGrok", "openMuse"].includes(id));
    if (key !== "escape" && key !== "mod+f") cleanup(keymap.bind(key, () => { stripOpen.set(false); if (agent) dispatch(agent[1]); }));
  }
  effect(() => {
    // NSH's local monitor consumes bindings before native keyDown. Remove this
    // binding while preview owns Escape so its real key/down-up path receives it.
    if (hooks.previewOwnsEscape?.() && !hooks.blocked() && !stripOpen()) return;
    return keymap.bind("escape", () => {
      if (stripOpen()) stripOpen.set(false);
      hooks.escape();
    }, { when: "always" });
  });
  cleanup(host.on("command", (data) => {
    const { id, args } = data as { id?: unknown; args?: { query?: string } };
    if (typeof id === "string") dispatch(id, args);
  }));
  return { dispatch, stripOpen, stripIndex, stripItems, toggleStrip, activateStrip };
}
