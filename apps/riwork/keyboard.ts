/** Commands whose native surface is still supplied by a focused chat. */
export const DEFERRED_COMMANDS = new Set([
  "interruptChat",
  "toggleDictation", "chatEscape", "findInChat", "findNext", "findPrevious",
  "toggleTranscriptMode", "expandTranscript", "collapseTranscript",
]);

/** Chat contexts share a chord with window commands; retain that distinction. */
export const DEFERRED_KEYMAP = [
  ["mod+shift+c", "openCodex", "window"],
  ["mod+shift+l", "openClaude", "window"],
  ["mod+shift+g", "openGrok", "window"],
  ["mod+shift+u", "openMuse", "window"],
  ["mod+alt+shift+c", "newChat", "window"],
  ["mod+n", "createProject", "window"],
  ["mod+f", "search", "window"],
  ["mod+.", "interruptChat", "chat"],
  ["escape", "chatEscape", "chat"],
  ["ctrl+alt+d", "toggleDictation", "chat"],
  ["mod+shift+v", "toggleTranscriptMode", "chat"],
  ["mod+alt+right", "expandTranscript", "chat"],
  ["mod+alt+left", "collapseTranscript", "chat"],
  ["mod+f", "findInChat", "chat"],
  ["mod+g", "findNext", "chat"],
  ["mod+shift+g", "findPrevious", "chat"],
  ["mod+f", "findInChat", "chat-input"],
  ["mod+g", "findNext", "chat-input"],
  ["mod+shift+g", "findPrevious", "chat-input"],
] as const;

export const TAB_CYCLE_KEYS = [
  ["ctrl+tab", 1], ["ctrl+shift+tab", -1],
  // NSH requires the unshifted physical key: these are also ⌘} / ⌘{.
  ["mod+shift+]", 1], ["mod+shift+[", -1],
] as const;

/** One declaration of the Phase-1 window map, including menu-backed actions.
 * AppKit consumes menu equivalents before NshView and emits the same command;
 * headless engines use these bindings directly. Neither path dispatches twice.
 */
export const WINDOW_KEYMAP = [
  ["mod+,", "openSettings"], ["mod+alt+a", "openProjectSettings"],
  ["mod+shift+s", "openSchedules"], ["mod+shift+e", "openFiles"],
  ["mod+alt+s", "openSessions"], ["mod+shift+p", "openPreview"],
  ["mod+shift+m", "gatherTabs"], ["mod+alt+l", "defaultLayout"],
  ["mod+t", "newTab"], ["mod+d", "splitRight"],
  ["mod+shift+d", "splitDown"], ["mod+w", "closeTab"],
  ["mod+shift+w", "closePane"], ["mod+b", "openProjects"],
  ["mod+shift+f", "toggleFocusMode"], ["mod+shift+n", "newProjectWindow"],
  ["mod+alt+o", "openProjectOrchestrator"],
  ["mod+1", "selectTab1"], ["mod+2", "selectTab2"],
  ["mod+3", "selectTab3"], ["mod+4", "selectTab4"],
  ["mod+5", "selectTab5"], ["mod+6", "selectTab6"],
  ["mod+7", "selectTab7"], ["mod+8", "selectTab8"], ["mod+9", "selectTab9"],
  ...TAB_CYCLE_KEYS.map(([key, direction]) => [key, direction === 1 ? "nextTab" : "previousTab"] as const),
] as const;

/** App-owned equivalents never registered in a TSX window keymap. */
export const HOST_KEYMAP = [
  ["mod+alt+g", "openGlobalDashboard"], ["mod+shift+o", "openGlobalChat"],
  ["mod+q", "quit"], ["mod+=", "biggerText"], ["mod+-", "smallerText"],
  ["mod+0", "actualSize"],
] as const;

export const PANEL_COMMANDS: Readonly<Record<string, string>> = {
  openProjects: "projects", openFiles: "files", openPreview: "preview",
  openSessions: "shells", openSchedules: "schedules", openSettings: "settings",
  openProjectSettings: "project_settings",
};

/** A strip menu dismisses before a modifier command. Modal actions stay inert. */
export function commandAllowed(command: string, modalOpen: boolean, dismissStrip: () => void): boolean {
  dismissStrip();
  return !modalOpen && !DEFERRED_COMMANDS.has(command);
}
