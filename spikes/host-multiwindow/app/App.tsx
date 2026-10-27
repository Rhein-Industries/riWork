import { List, Text, View, host, mount, signal } from "nsh";
import { Button, TextView } from "nsh/base";
import { Probe, ReplayCounter } from "riwork/native";
import { DEFAULT_CHUNK_BYTES, FIXTURE_BYTES, TRANSCRIPT } from "./fixture";
import { REVISION } from "./Revision";

interface ReplayOptions {
  windowId?: string;
  label?: string;
  historyRows?: number;
  chunkBytes?: number;
  intervalMs?: number;
}
interface Message { id: number; role: string; markdown: string }

function summary(values: number[]) {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    count: values.length,
    medianMs: sorted[Math.floor(sorted.length / 2)] ?? 0,
    p95Ms: sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))] ?? 0,
    maxMs: sorted[sorted.length - 1] ?? 0,
  };
}

function App() {
  const params = host.params as ReplayOptions;
  const windowId = params.windowId ?? "headless";
  const nativeIdentity = Probe.identity(windowId);
  const historyRows = params.historyRows ?? 200;
  const rows: Message[] = Array.from({ length: historyRows }, (_, id) => ({
    id,
    role: id % 2 ? "assistant" : "user",
    markdown: `### Isolated message ${id}\n\nGenerated history for **window ${windowId}**.\n\n- Local fixture only\n- Rust + TSX list recycling`,
  }));
  rows.push({ id: -1, role: "assistant (streaming)", markdown: "" });
  const markdown = signal("");
  const status = signal("Ready");
  const visibleChunks = signal(0);
  let counter = new ReplayCounter();
  let runId = 0;
  let streamedMarkdownReads = 0;
  let streamedLayoutCount = 0;
  let streamedHeight = 0;

  function streamedMarkdown() {
    streamedMarkdownReads += 1;
    return markdown();
  }

  function post(topic: string, extra: object = {}) {
    if (host.available()) {
      host.post(topic, {
        windowId, nativeIdentity, revision: REVISION, historyRows,
        fixtureBytes: FIXTURE_BYTES, chunkCount: counter.chunks,
        bytes: counter.bytes, checksum: counter.checksum,
        streamedMarkdownReads, streamedLayoutCount, streamedHeight, ...extra,
      });
    }
  }

  function replay(options: ReplayOptions = {}) {
    const activeRun = ++runId;
    const chunkBytes = Math.max(1, Math.floor(options.chunkBytes ?? params.chunkBytes ?? DEFAULT_CHUNK_BYTES));
    const intervalMs = Math.max(0, options.intervalMs ?? params.intervalMs ?? 0);
    counter = new ReplayCounter();
    streamedMarkdownReads = 0;
    streamedLayoutCount = 0;
    markdown.set("");
    visibleChunks.set(0);
    status.set("Streaming");
    const startedAt = performance.now();
    let lastTick = startedAt;
    let offset = 0;
    const updateMs: number[] = [];
    const tickGapMs: number[] = [];

    function tick() {
      if (activeRun !== runId) return;
      const start = performance.now();
      tickGapMs.push(start - lastTick);
      lastTick = start;
      // The baseline fixture is ASCII: JS indices and UTF-8 byte boundaries agree.
      const chunk = TRANSCRIPT.slice(offset, offset + chunkBytes);
      offset += chunk.length;
      const chunks = counter.append(chunk);
      markdown.set(TRANSCRIPT.slice(0, offset));
      visibleChunks.set(chunks);
      updateMs.push(performance.now() - start);
      if (chunks % 100 === 0) post("progress", { elapsedMs: performance.now() - startedAt });
      if (offset < TRANSCRIPT.length) {
        setTimeout(tick, intervalMs);
      } else {
        // A separate timer lets the final reactive update reach the engine commit.
        setTimeout(() => {
          if (activeRun !== runId) return;
          const elapsedMs = performance.now() - startedAt;
          status.set(`Done: ${counter.bytes} bytes / ${counter.chunks} chunks / ${counter.checksum}`);
          post("done", {
            elapsedMs, chunkBytes, intervalMs,
            chunksPerSecond: counter.chunks * 1000 / elapsedMs,
            bytesPerSecond: counter.bytes * 1000 / elapsedMs,
            // Synchronous signal/native-call cost, NOT drawing or presentation time.
            updateMs: summary(updateMs),
            updateFirst100Ms: summary(updateMs.slice(0, 100)),
            updateLast100Ms: summary(updateMs.slice(-100)),
            tickGapMs: summary(tickGapMs),
          });
        }, 0);
      }
    }
    setTimeout(tick, intervalMs);
  }

  host.on("replay", (value) => replay((value ?? {}) as ReplayOptions));
  host.on("stream/replay", (value) => replay((value ?? {}) as ReplayOptions));
  host.on("probe", () => post("probe", { status: status() }));
  setTimeout(() => post("ready"), 0);

  return (
    <View style={{ flex: 1, backgroundColor: "#f7f8fa" }}>
      <View style={{ padding: 12, gap: 6 }}>
        <Text style={{ fontSize: 20, fontWeight: "600" }}>{`Window ${windowId} · ${REVISION}`}</Text>
        <Text testID="native-identity">{nativeIdentity}</Text>
        <Text testID="status">{status()}</Text>
        <Text testID="chunk-count">{`Chunks: ${visibleChunks()}`}</Text>
        <Button testID="replay" onPress={() => replay({ intervalMs: 0 })}>
          <Text>Replay fixture</Text>
        </Button>
      </View>
      <List
        testID="transcript"
        each={rows}
        initialScrollIndex={historyRows}
        estimatedItemSize={150}
        maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 200 }}
        style={{ flex: 1 }}
        contentStyle={{ padding: 12, gap: 12 }}
      >
        {(item) => (
          <View style={{ padding: 12, backgroundColor: "#ffffff", borderRadius: 8 }}>
            <Text style={{ fontSize: 12, color: "#555555" }}>{item().role}</Text>
            <TextView
              testID={item().id === -1 ? "streamed-markdown" : `history-${item().id}`}
              markdown={item().id === -1 ? streamedMarkdown() : item().markdown}
              options={{ streaming: true }}
              selectable={true}
              onLayout={(bounds) => {
                if (item().id === -1) {
                  streamedLayoutCount += 1;
                  streamedHeight = bounds.size.height;
                }
              }}
            />
          </View>
        )}
      </List>
    </View>
  );
}

mount(App);
