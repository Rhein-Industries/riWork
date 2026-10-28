import { expect, test } from "nsh/test";
import { DEFERRED_COMMANDS, DEFERRED_KEYMAP, HOST_KEYMAP, WINDOW_KEYMAP } from "../keyboard";

test("P1-KEYS-01 no key is bound twice and every GPUI chord is present", () => {
  const declared = [
    ...WINDOW_KEYMAP.map(([key]) => `window:${key}`),
    ...HOST_KEYMAP.map(([key]) => `window:${key}`),
    ...DEFERRED_KEYMAP.map(([key, , scope]) => `${scope}:${key}`),
  ];
  expect(new Set(declared).size).toBe(declared.length);
  // Frozen main.rs chord table. NSH canonicalizes ⌘{ / ⌘} to the physical
  // bracket key plus Shift, so each alias has one executable registration.
  const windowKeys = [
    "mod+,", "mod+alt+a", "mod+shift+s", "mod+shift+e", "mod+alt+s", "mod+shift+p",
    "mod+shift+m", "mod+alt+l", "mod+q", "mod+n", "mod+t", "mod+d", "mod+shift+d",
    "mod+w", "mod+shift+w", "ctrl+tab", "ctrl+shift+tab", "mod+shift+]", "mod+shift+[",
    "mod+1", "mod+2", "mod+3", "mod+4", "mod+5", "mod+6", "mod+7", "mod+8", "mod+9",
    "mod+b", "mod+f", "mod+shift+f", "mod+shift+o", "mod+alt+o", "mod+alt+g", "mod+shift+n",
    "mod+shift+c", "mod+shift+l", "mod+shift+g", "mod+shift+u", "mod+alt+shift+c",
    "mod+=", "mod+-", "mod+0",
  ];
  const expected = [
    ...windowKeys.map((key) => `window:${key}`),
    ...["mod+.", "escape", "ctrl+alt+d", "mod+shift+v", "mod+alt+right", "mod+alt+left", "mod+f", "mod+g", "mod+shift+g"].map((key) => `chat:${key}`),
    ...["mod+f", "mod+g", "mod+shift+g"].map((key) => `chat-input:${key}`),
  ];
  expect(declared.slice().sort()).toEqual(expected.sort());
  expect(Array.from(DEFERRED_COMMANDS).sort()).toEqual([
    "openCodex", "openClaude", "openGrok", "openMuse", "newChat",
    "createProject", "search", "interruptChat", "chatEscape", "toggleDictation", "findInChat",
    "findNext", "findPrevious", "toggleTranscriptMode", "expandTranscript", "collapseTranscript",
    "appPreview", "sessions", "existingSessions", "existingClaudeChats", "existingMuseChats",
    "handOff",
  ].sort());
});

test("P1-KEYS-03 app equivalents are never installed in a TSX window keymap", () => {
  const windowKeys = new Set<string>(WINDOW_KEYMAP.map(([key]) => key));
  for (const [key] of HOST_KEYMAP) expect(windowKeys.has(key)).toBe(false);
});
