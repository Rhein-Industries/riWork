import { cleanup, effect, host, keymap, signal } from "nsh";
import type { LayoutSession } from "riwork/native";
import { commandAllowed, DEFERRED_KEYMAP, PANEL_COMMANDS, WINDOW_KEYMAP } from "./keyboard";
import { activePane, cycledTab, drawnTabs, numberedTab, type HostParams, type WorkspaceSnapshot } from "./model";

/** Workspace-owned effects that the dispatcher routes but does not implement. */
export interface CommandHooks {
  /** Panel editors and project switches block window commands like a modal. */
  blocked: () => boolean;
  /** Shell tabs close through the terminal's Ask/Detach/Exit behaviour. */
  closeShell: (shellId: string) => void;
  /** Create a local shell through the async Sessions service. */
  newShell: () => void;
  /** The window's current project, which a panel project switch can change. */
  projectId: () => string | undefined;
}

/** Both native menus and key bindings enter this one window-local dispatcher. */
export function workspaceCommands(
  session: LayoutSession,
  snapshot: () => WorkspaceSnapshot,
  action: (run: () => unknown) => void,
  hooks: CommandHooks,
) {
  // The tab whose close confirmation is open; shells offer Detach and Exit.
  const closeDialog = signal<string | null>(null);
  const stripOpen = signal(false);
  const stripIndex = signal(0);
  const params = host.params as unknown as HostParams;
  const focusMode = signal(params.restore?.focusMode ?? false);
  const focusCentered = params.restore?.focusCentered ?? false;
  effect(() => session.setFocusMode(focusMode(), focusCentered));
  function stripItems() {
    const pane = activePane(snapshot());
    return pane ? drawnTabs(pane) : [];
  }
  function toggleStrip() {
    if (closeDialog()) return;
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
  function dispatch(id: string) {
    if (!commandAllowed(id, closeDialog() !== null || hooks.blocked(), () => stripOpen.set(false))) return;
    const panel = PANEL_COMMANDS[id];
    if (panel) { action(() => session.openPanel(panel)); return; }
    const pane = activePane(snapshot());
    if (/^selectTab[1-9]$/.test(id)) {
      // Until the shared strip service is installed, this is pane drawn order.
      const tab = pane && numberedTab(pane, Number(id.slice(-1)), false);
      if (tab) action(() => session.selectPanel(tab.id));
    } else if (id === "nextTab" || id === "previousTab") {
      const tab = pane && cycledTab(pane, pane.activeTab, id === "nextTab" ? 1 : -1);
      if (tab) action(() => session.selectPanel(tab.id));
    } else if (id === "gatherTabs") action(() => session.gather());
    else if (id === "defaultLayout") action(() => session.defaultLayout());
    else if (id === "splitRight" || id === "splitDown") action(() => session.splitActive(id === "splitDown"));
    else if (id === "closePane") action(() => session.closeActive(true));
    else if (id === "newTab" || id === "newShell") hooks.newShell();
    else if (id === "closeTab") {
      const selected = pane?.tabs.find((tab) => tab.id === pane.activeTab);
      if (!selected) return;
      if (selected.id.startsWith("shell:")) hooks.closeShell(selected.id.slice(6));
      else if (selected.kind === "panel") action(() => session.closeActive(false));
      else if (!pane?.locked) closeDialog.set(selected.id);
      else action(() => session.closeActive(false));
    } else if (id === "toggleFocusMode") focusMode.set(!focusMode());
    else if (id === "newProjectWindow") {
      void host.call("window.open", { kind: "project", projectId: hooks.projectId() ?? params.projectId }).catch(() => {});
    }
  }
  for (const [key, id] of WINDOW_KEYMAP) cleanup(keymap.bind(key, () => dispatch(id)));
  // Panel search owns ⌘F; the remaining deferred chords stay inert.
  for (const key of new Set(DEFERRED_KEYMAP.map(([key]) => key))) {
    if (key !== "escape" && key !== "mod+f") cleanup(keymap.bind(key, () => { stripOpen.set(false); }));
  }
  cleanup(keymap.bind("escape", () => {
    if (stripOpen()) stripOpen.set(false);
    else if (closeDialog()) closeDialog.set(null);
  }));
  cleanup(host.on("command", (data) => {
    const id = (data as { id?: unknown }).id;
    if (typeof id === "string") dispatch(id);
  }));
  return {
    dispatch, closeDialog, stripOpen, stripIndex, stripItems, toggleStrip, activateStrip, focusMode,
    confirmClose: () => { closeDialog.set(null); action(() => session.closeActive(false)); },
  };
}
