/** Native workspace measurements, in logical points. */
export const WORKSPACE_METRICS = {
  header: 28, divider: 5, nativeButton: 22, buttonGap: 2, buttonRadius: 3,
  menuButton: 28, trafficLightInset: 92, lockAt: 108, mainAt: 136,
  mainWithLockAt: 152, focusAt: 180, panelCell: 32,
  statusDot: 7, idleDot: 3, tabClose: 16, tabFont: 10,
  closeDialogWidth: 380, allTabsWidth: 260, allTabsMaxHeight: 420,
  focusMaxWidth: 1100, focusBottomMargin: 0.3,
} as const;

export const STATUS_COLORS = {
  working: "#58b8ef", waiting: "#e7b65b", error: "#ed7777",
  done: "#747d8a", idle: "#8791a0", stopped: "#8791a0",
} as const;

export function headerHeight(textPoints = 11): number {
  return WORKSPACE_METRICS.header * Math.max(1, textPoints / 11);
}

export function mainMarkerThreshold(theme = "native", icons = false): number {
  return theme === "native" || icons ? WORKSPACE_METRICS.mainAt : WORKSPACE_METRICS.mainWithLockAt;
}

export function sessionCellWidth(title: string): number {
  // 8 leading + 7 dot + 6 gap + title (capped at 220) + 6 gap + 16 X + 4 trailing.
  return 47 + Math.min(220, title.length * 5.7);
}

/** Minimal scroll needed to reveal the selected cell, including after rename. */
export function revealOffset(start: number, width: number, viewport: number, offset: number): number {
  if (start < offset) return Math.max(0, start);
  if (start + width > offset + viewport) return Math.max(0, start + width - viewport);
  return offset;
}
