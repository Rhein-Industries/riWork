/** The app chords of a project's page on iPhone and iPad, by command id: one table for the page's
 * own keymap (while TSX holds the keyboard) and for the terminal's `command` event (schema 1.1,
 * `terminal_view.ts`: the iOS terminal sends the id of a chord typed while it holds the keyboard,
 * and of the Clicks Tab-prefix action for it). Shared code: it imports nothing native. */
export type AppChord =
  | { type: "select"; index: number }
  | { type: "next" }
  | { type: "previous" }
  | { type: "newTerminal" }
  | { type: "back" }
  | { type: "openTab" };

/** Command id, keystroke (NSH `keymap`) and what it does. ⌘K, ⌘/ and the hotkeys stay inside the terminal. */
export const APP_CHORDS: readonly (readonly [id: string, keys: string, chord: AppChord])[] = [
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => [`tab.select.${n}`, `mod+${n}`, { type: "select", index: n - 1 }] as const),
  ["tab.previous", "mod+shift+[", { type: "previous" }],
  ["tab.next", "mod+shift+]", { type: "next" }],
  ["terminal.new", "mod+n", { type: "newTerminal" }],
  ["nav.back", "mod+shift+p", { type: "back" }],
  ["tab.open", "mod+shift+o", { type: "openTab" }],
];

/** The chord a terminal `command` id stands for; null for one this table does not have. */
export function appChord(id: string): AppChord | null {
  return APP_CHORDS.find(([name]) => name === id)?.[2] ?? null;
}
