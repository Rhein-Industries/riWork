import { expect, test } from "nsh/test";
import { latest, namedWorkspace, workspace } from "./workspace-test-support";
import { stripTabs } from "../model";
import { sessionCellWidth } from "../metrics";

test("P1-DRAG-01 real DockArea drag moves a tab between groups and selects its target", async () => {
  const app = await workspace("drag-group");
  try {
    await app.drag("id:tab:shell:S3", { x: 350, y: -200 });
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 2)?.tabs.some((tab) => tab.id === "shell:S3")).toBe(true);
    expect(latest(app).snapshot.activePane).toBe(2);
    expect(app.shows("id:panel:shell:S3 >> text:mount 1")).toBe(true);
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 3)?.activeTab).toBe("panel:tasks");
  } finally { app.dispose(); }
});
test("P1-DRAG-01 shared strip drag publishes a persistent adjacent order without remounting", async () => {
  const app = await namedWorkspace("Alpha", "strip-drag");
  try {
    const id = `shell:${latest(app).manifest!.aliases["Alpha-S3"]}`;
    await app.drag(`id:tab-${id}`, { x: -sessionCellWidth("Alpha-S1") - sessionCellWidth("Alpha-S2"), y: 0 });
    await app.waitUntil(() => stripTabs(latest(app).snapshot).filter((tab) => tab.kind !== "panel")[0]?.id === id);
    expect(app.texts().includes("mount 2")).toBe(false);
    expect(app.shows(`id:panel:${id} >> text:mount 1`)).toBe(true);
    const peer = await namedWorkspace("Alpha", "strip-drag-peer");
    try { expect(stripTabs(latest(peer).snapshot).filter((tab) => tab.kind !== "panel")[0]?.id).toBe(id); } finally { peer.dispose(); }
    await app.drag(`id:tab-${id}`, { x: sessionCellWidth("Alpha-S1"), y: 0 });
    await app.waitUntil(() => stripTabs(latest(app).snapshot).filter((tab) => tab.kind !== "panel")[1]?.id === id);
    await app.rightClick(`id:tab-${id}`); await app.chooseMenuItem("Move right");
    await app.waitUntil(() => stripTabs(latest(app).snapshot).filter((tab) => tab.kind !== "panel")[2]?.id === id);
  } finally { app.dispose(); }
});
test("P1-DRAG-02/04 a native right-edge drop creates a pane without remounting", async () => {
  const app = await workspace("drag-edge");
  try {
    await app.drag("id:tab:shell:S3", { x: -60, y: -200 });
    expect(latest(app).snapshot.panes.length).toBe(4);
    expect(app.shows("id:panel:shell:S3 >> text:mount 1")).toBe(true);
    expect(app.texts().includes("mount 2")).toBe(false);
  } finally { app.dispose(); }
});
test("P1-DRAG-03 no cross-window tab-transfer calls are served by the frontend", async () => {
  const app = await workspace("window-local-drag");
  const peer = await workspace("window-local-peer");
  try {
    const before = latest(peer);
    await app.drag("id:tab:shell:S3", { x: 350, y: -200 });
    expect(latest(peer)).toEqual(before);
    expect(app.host.calls().some((call) => call.name === "tab.transfer" || call.name === "tab.receive")).toBe(false);
  } finally { peer.dispose(); app.dispose(); }
});
