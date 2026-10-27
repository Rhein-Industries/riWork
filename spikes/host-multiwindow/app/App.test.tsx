import { expect, open, test } from "nsh/test";
import { TRANSCRIPT, FIXTURE_BYTES, FIXTURE_CHUNKS, DEFAULT_CHUNK_BYTES } from "./fixture";

test("uses the same deterministic transcript as the GPUI baseline", () => {
  expect(TRANSCRIPT.length).toBe(FIXTURE_BYTES);
  expect(Math.ceil(TRANSCRIPT.length / DEFAULT_CHUNK_BYTES)).toBe(FIXTURE_CHUNKS);
  expect(TRANSCRIPT.startsWith("## Part 0\n\n")).toBe(true);
  expect(TRANSCRIPT.endsWith("| src/lib.rs | 59 |\n\n")).toBe(true);
});

test("isolates host params and native state while replaying all 475 chunks", async () => {
  const options = (windowId: string) => ({
    platform: "macos" as const, width: 760, height: 860,
    host: {
      params: { windowId, historyRows: 200, chunkBytes: 40, intervalMs: 0 },
      handlers: { ready: null, progress: null, done: null, probe: null },
    },
  });
  const app = await open("./App.tsx", options("A"));
  const peer = await open("./App.tsx", options("B"));
  expect(app.texts()).toContain("riwork/native:0.3.0:A");
  expect(peer.texts()).toContain("riwork/native:0.3.0:B");
  expect(app.shows("id:transcript")).toBe(true);
  await app.host.send("replay", { intervalMs: 0 });
  await app.waitUntil(() => app.texts().includes("Done: 18980 bytes / 475 chunks / 4f4771e9"), { timeout: 30_000 });
  expect(app.texts()).toContain("Chunks: 475");
  expect(peer.texts()).toContain("Chunks: 0");
  expect(peer.texts()).toContain("Ready");
  const done = app.host.posts().find((message) => message.name === "done")?.data as {
    windowId: string; chunkCount: number; bytes: number; checksum: string; revision: string;
    streamedMarkdownReads: number; streamedLayoutCount: number; streamedHeight: number;
  };
  expect(done.windowId).toBe("A");
  expect(done.chunkCount).toBe(FIXTURE_CHUNKS);
  expect(done.bytes).toBe(FIXTURE_BYTES);
  expect(done.checksum).toBe("4f4771e9");
  expect(done.revision).toBe("revision-1");
  expect(done.streamedMarkdownReads > 100).toBe(true);
  expect(done.streamedLayoutCount > 0).toBe(true);
  expect(done.streamedHeight > 1000).toBe(true);
  expect(app.texts()).toContain("Part 59");
  expect(app.texts()).toContain("src/lib.rs");
  expect(app.texts()).toContain("59");
  await peer.host.send("probe", {});
  const probe = peer.host.posts().find((message) => message.name === "probe")?.data as {
    windowId: string; chunkCount: number;
  };
  expect(probe.windowId).toBe("B");
  expect(probe.chunkCount).toBe(0);
}, 60_000);
