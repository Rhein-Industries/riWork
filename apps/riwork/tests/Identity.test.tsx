import { expect, test } from "nsh/test";
import { latest, workspace } from "./workspace-test-support";

test("P1-DRAG-04 panel mounts survive native group moves, focus and resize", async () => {
  const app = await workspace("mount-identity");
  try {
    expect(app.texts()).toContain("mount 1");
    await app.drag("id:tab:shell:S3", { x: 350, y: -200 });
    expect(latest(app).snapshot.activePane).toBe(2);
    expect(app.shows("id:panel:shell:S3 >> text:mount 1")).toBe(true);
    expect(app.texts().includes("mount 2")).toBe(false);
    await app.keyboard.press("mod+shift+f");
    expect(app.shows("id:panel:shell:S3 >> text:mount 1")).toBe(true);
    expect(app.texts().includes("mount 2")).toBe(false);
    await app.keyboard.press("mod+shift+f");
    await app.setViewport(1440, 900);
    expect(app.shows("id:panel:shell:S3 >> text:mount 1")).toBe(true);
    expect(app.texts().includes("mount 2")).toBe(false);
    expect(app.texts()).toContain("visible: true");
  } finally { app.dispose(); }
});
