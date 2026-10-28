import { space, type InterfaceTheme } from "../appearance";

/** Shared reading measure keeps the draft and conversation on one axis. */
export const CHAT_COLUMN_WIDTH = 960;

export function chatGutter(width: number, scale: number): number {
  return space(width > 0 && width < 520 ? 12 : 24, scale);
}

export function chatColumn(width: number, scale: number) {
  return { width: "100%" as const, maxWidth: CHAT_COLUMN_WIDTH, alignSelf: "center" as const,
    paddingHorizontal: chatGutter(width, scale) };
}

export function chatHairline(theme: InterfaceTheme): string {
  const divider = theme.palette.divider;
  return /^#[\da-f]{6}$/i.test(divider) ? `${divider}88` : divider;
}
