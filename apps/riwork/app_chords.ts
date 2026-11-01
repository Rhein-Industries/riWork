/** The app chords of a project's page on iPhone and iPad, by command id: one table for the page's
 * own keymap (while TSX holds the keyboard), for the terminal's `command` event (schema 1.1,
 * `terminal_view.ts`, docs/nsh-contracts.md §3: the iOS terminal sends the id of a chord typed while
 * it holds the keyboard, and of the Clicks Tab-prefix action for it) and for Swift's `keyboard.command`
 * event (the same ids from the root key host and the native chat). The ids are design §2.5's; the
 * module is not `keyboard.ts`, which is the Mac's command keymap. Shared code: it imports nothing native. */
export type AppChord =
  | { type: "select"; index: number }
  | { type: "next" }
  | { type: "previous" }
  | { type: "newTerminal" }
  | { type: "back" }
  | { type: "projects" }
  | { type: "display" }
  | { type: "picker" };

/** Command id, keystroke (NSH `keymap`; null for a Clicks Tab-prefix action with no ⌘ chord) and what it
 * does. ⌘K, ⌘/ and the hotkeys stay inside the terminal; `chat.model` (Clicks Tab M) is the native chat's. */
export const APP_CHORDS: readonly (readonly [id: string, keys: string | null, chord: AppChord])[] = [
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => [`tab.select.${n}`, `mod+${n}`, { type: "select", index: n - 1 }] as const),
  ["tab.prev", "mod+shift+[", { type: "previous" }],
  ["tab.next", "mod+shift+]", { type: "next" }],
  ["terminal.new", "mod+n", { type: "newTerminal" }],
  ["nav.back", "mod+shift+p", { type: "back" }],
  ["nav.projects", null, { type: "projects" }],
  ["display.open", "mod+shift+,", { type: "display" }],
  ["picker.open", "mod+shift+o", { type: "picker" }],
];

/** The chord a command id stands for; null for one this table does not have. */
export function appChord(id: string): AppChord | null {
  return APP_CHORDS.find(([name]) => name === id)?.[2] ?? null;
}
