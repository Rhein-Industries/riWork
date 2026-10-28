export type SettingsWidthClass = "narrow" | "medium" | "wide";
export function settingsLayout(width: number) {
  const kind: SettingsWidthClass = width >= 1200 ? "wide" : width >= 700 ? "medium" : "narrow";
  return { kind, maxWidth: kind === "wide" ? 1320 : kind === "medium" ? 820 : undefined,
    page: kind === "wide" ? 28 : kind === "medium" ? 24 : 12,
    card: kind === "wide" ? 18 : kind === "medium" ? 16 : 12,
    gap: kind === "wide" ? 16 : kind === "medium" ? 14 : 10 };
}
export function themeColumns(width: number, scale: number): number {
  const layout = settingsLayout(width);
  if (layout.kind === "narrow") return 1;
  const content = Math.min(width - 2 * layout.page, layout.maxWidth ?? Infinity);
  const column = layout.kind === "wide" ? (content - layout.gap) / 2 : content;
  const inner = Math.max(0, column - 2 * (layout.card + 1));
  return Math.max(2, Math.min(3, Math.floor((inner + 8) / (220 * scale + 8))));
}
export const SETTINGS_SECTIONS = [
  { number: "01", title: "Computer use", description: "Let RiWork agents operate desktop apps through Cua.ai." },
  { number: "02", title: "Codex accounts", description: "The account new Codex sessions start with." },
  { number: "03", title: "Appearance", description: "Choose a theme and text size, or sync with Ghostty." },
  { number: "04", title: "Files", description: "How the Files tree and its Preview pane open together." },
  { number: "05", title: "Agent sessions", description: "How agent sessions draw, how orchestrators run, and dictation." },
  { number: "06", title: "Windows", description: "How project windows open." },
  { number: "07", title: "Status bar", description: "Choose what appears, which side it sits on, and its order." },
  { number: "08", title: "Remote", description: "Control other Macs' shells from here, or let another Mac control this one." },
  { number: "09", title: "Import from Orca", description: "Bring projects and worktrees over from Orca once." },
] as const;
export function sectionColumns(width: number): string[][] {
  const sections = SETTINGS_SECTIONS.map((section) => section.number);
  return settingsLayout(width).kind === "wide" ? [sections.slice(0, 4), sections.slice(4)] : [sections];
}
export function modelIdError(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return "Type a model ID first.";
  return [...trimmed].length > 100 || /[\x00-\x1f\x7f-\x9f]/.test(trimmed) ? "A model ID has at most 100 characters and no control characters." : null;
}
