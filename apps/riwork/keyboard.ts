/** Deferred Phase-1 seams, acceptance §8. These never start a deferred surface. */
export const DEFERRED_COMMANDS = new Set([
  "openCodex", "openClaude", "openGrok", "openMuse", "newChat",
  "openProjectOrchestrator", "createProject", "search", "interruptChat",
  "toggleDictation", "chatEscape", "findInChat", "findNext", "findPrevious",
  "toggleTranscriptMode", "expandTranscript", "collapseTranscript", "appPreview",
  "sessions", "existingSessions", "existingClaudeChats", "existingMuseChats",
  "openGlobalOrchestrator", "handOff",
]);

/** Chat contexts share a chord with window commands; retain that distinction. */
export const DEFERRED_KEYMAP = [
  ["mod+shift+c", "openCodex", "window"],
  ["mod+shift+l", "openClaude", "window"],
  ["mod+shift+g", "openGrok", "window"],
  ["mod+shift+u", "openMuse", "window"],
  ["mod+alt+shift+c", "newChat", "window"],
  ["mod+alt+o", "openProjectOrchestrator", "window"],
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
