import { expect, test } from "nsh/test";
import { STATUS_COLORS, WORKSPACE_METRICS as M, headerHeight, mainMarkerThreshold, revealOffset, sessionCellWidth } from "../metrics";
test("P1-PANE-12 / P1-STRIP-02 native pane and strip metrics", () => {
  expect([M.header, M.divider, M.nativeButton, M.buttonGap, M.buttonRadius, M.menuButton]).toEqual([28, 5, 22, 2, 3, 28]);
  expect([M.trafficLightInset, M.lockAt, M.mainAt, M.mainWithLockAt, M.focusAt]).toEqual([92, 108, 136, 152, 180]);
  expect([M.panelCell, M.closeDialogWidth, M.allTabsWidth, M.allTabsMaxHeight]).toEqual([32, 380, 260, 420]);
  expect(String(STATUS_COLORS.waiting) === String(STATUS_COLORS.error)).toBe(false);
  expect(headerHeight(11)).toBe(28);
  expect(headerHeight(22)).toBe(56);
  expect([mainMarkerThreshold("native"), mainMarkerThreshold("riwork", true), mainMarkerThreshold("riwork", false)]).toEqual([136, 136, 152]);
});
test("P1-STRIP-04 selected cell reveals after width, position and rename changes", () => {
  expect(revealOffset(600, 120, 500, 0)).toBe(220);
  expect(revealOffset(20, 120, 500, 220)).toBe(20);
  expect(revealOffset(300, 120, 500, 0)).toBe(0);
  const renamedWidth = sessionCellWidth("A long renamed session title");
  expect(Math.round(revealOffset(300, renamedWidth, 400, 0) * 100)).toBe(Math.round((renamedWidth - 100) * 100));
});
