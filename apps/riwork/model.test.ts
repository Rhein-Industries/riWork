import { expect, test } from "nsh/test";
import {
  activePane, createTitleUpdater, cycledTab, drawnTabs, findPanel, mainPaneSummary,
  numberedTab, panelTitle, windowTitle,
} from "./model";
import type { PaneModel, WorkspaceSnapshot } from "./model";

function pane(): PaneModel {
  return {
    id: 2, locked: false, main: true, active: true, activeTab: "chat:second",
    tabs: [
      { id: "panel:projects", kind: "panel", title: "Projects" },
      { id: "chat:second", kind: "chat", title: "Second" },
      { id: "shell:first", kind: "shell", title: "First" },
      { id: "chat:carried", kind: "chat", title: "Carried" },
    ],
  };
}

test("number shortcuts consume drawn order and skip panels only with shared membership", () => {
  const subject = pane();
  expect(numberedTab(subject, 1, true)?.id).toBe("chat:second");
  expect(numberedTab(subject, 2, true)?.id).toBe("shell:first");
  expect(numberedTab(subject, 9, true)?.id).toBe("chat:carried");
  expect(numberedTab(subject, 1, false)?.id).toBe("panel:projects");
  expect(numberedTab(subject, 9, false)?.id).toBe("chat:carried");
  expect(numberedTab(subject, 8, true)).toBe(undefined);
  expect(numberedTab(subject, 0, true)).toBe(undefined);
  expect(numberedTab(subject, 10, true)).toBe(undefined);
  expect(numberedTab(subject, 1.5, true)).toBe(undefined);
  expect(numberedTab(subject, 1, true, true)).toBe(undefined);
});

test("tab cycling wraps in drawn order and respects a modal", () => {
  const subject = pane();
  expect(cycledTab(subject, "chat:carried", 1)?.id).toBe("panel:projects");
  expect(cycledTab(subject, "panel:projects", -1)?.id).toBe("chat:carried");
  expect(cycledTab(subject, "chat:second", 1)?.id).toBe("shell:first");
  expect(cycledTab(subject, "chat:second", -1)?.id).toBe("panel:projects");
  expect(cycledTab(subject, "missing", 1)?.id).toBe("panel:projects");
  expect(cycledTab(subject, "chat:second", 1, true)).toBe(undefined);
});

test("empty sentinels never participate in tab shortcuts or the main-pane summary", () => {
  const subject = pane();
  subject.tabs = [{ id: "empty:2", kind: "empty", title: "" }];
  const snapshot: WorkspaceSnapshot = { panes: [subject], activePane: 2 };
  expect(drawnTabs(subject)).toEqual([]);
  expect(numberedTab(subject, 1, false)).toBe(undefined);
  expect(cycledTab(subject, undefined, 1)).toBe(undefined);
  expect(mainPaneSummary(snapshot)).toBe("empty");
});

test("main-pane summaries preserve active identity and lookup is pane-independent", () => {
  const subject = pane();
  const snapshot: WorkspaceSnapshot = { panes: [subject], activePane: 2 };
  expect(activePane(snapshot)?.id).toBe(2);
  expect(findPanel(snapshot, "chat:second")?.title).toBe("Second");
  expect(findPanel(snapshot, "missing")).toBe(undefined);
  expect(mainPaneSummary(snapshot)).toBe("Second · 4 tabs");
  subject.tabs = [subject.tabs[1]];
  expect(mainPaneSummary(snapshot)).toBe("Second");
  subject.main = false;
  expect(mainPaneSummary(snapshot)).toBe("none");
});

test("window titles name remote projects and update only when changed", () => {
  expect(windowTitle("RiWork")).toBe("RiWork · RiWork");
  expect(windowTitle("app", "Studio")).toBe("RiWork · app (on Studio)");
  expect(windowTitle(null, null, "global")).toBe("RiWork · Global");
  expect(windowTitle()).toBe("RiWork");
  const calls: string[] = [];
  const update = createTitleUpdater((title) => calls.push(title));
  update(windowTitle("app"));
  update(windowTitle("app"));
  update(windowTitle("app", "Studio"));
  expect(calls).toEqual(["RiWork · app", "RiWork · app (on Studio)"]);
});

test("panel labels preserve persisted legacy identities", () => {
  expect(panelTitle("panel:shells")).toBe("Sessions");
  expect(panelTitle("schedules")).toBe("Automations");
  expect(panelTitle("panel:project_settings")).toBe("Project settings");
  expect(panelTitle("panel:future_panel")).toBe("future_panel");
});
