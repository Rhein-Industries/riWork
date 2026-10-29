import { Scroll, Text, View, computed, effect, signal } from "nsh";
import { Input } from "nsh/base";
import type { ChatHandle, ChatPanel, ChatWorker } from "riwork/native";
import type { InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";
import { chatColumn, chatHairline } from "./design";

export function WorkerShelf(props: {
  handle: ChatHandle; panel: () => ChatPanel; visible: () => boolean; theme: () => InterfaceTheme;
  width?: () => number;
  onOpen?: (id: string, kind: "shell" | "chat") => void;
  onReveal: (id: string) => void; onError: (message: string) => void;
}) {
  const workers = signal<ChatWorker[]>([]), expanded = signal(false), selected = signal<string | null>(null);
  const message = signal(""), messaging = signal<string | null>(null), stopping = signal<string | null>(null), busy = signal<string | null>(null);
  const reports = computed(() => JSON.stringify(props.panel().workers ?? []));
  let refresh = () => {};
  effect(() => {
    const visible = props.visible(), snapshot = reports();
    const initial = JSON.parse(snapshot) as ChatWorker[];
    workers.set(initial);
    if (!visible || !initial.length) return;
    let disposed = false, reading = false, unchanged = 0, previous = "";
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      if (disposed || reading) return;
      clearTimeout(timer); reading = true;
      try {
        const rows = await props.handle.workers();
        if (disposed) return;
        const signature = JSON.stringify(rows);
        unchanged = signature === previous ? unchanged + 1 : 0; previous = signature;
        // An unchanged poll leaves the rows (and an open shelf) as they are.
        if (signature !== JSON.stringify(workers())) workers.set(rows);
        if (rows.some((row) => row.active)) timer = setTimeout(() => void read(), Math.min(30_000, 2_000 * 2 ** Math.min(unchanged, 4)));
      } catch (error) {
        if (!disposed) { props.onError(String(error)); timer = setTimeout(() => void read(), 30_000); }
      } finally { reading = false; }
    };
    refresh = () => { unchanged = 0; void read(); };
    void read();
    return () => { disposed = true; clearTimeout(timer); refresh = () => {}; };
  });
  const theme = props.theme;
  const counts = computed(() => ({
    total: workers().length,
    active: workers().filter((worker) => worker.active).length,
    waiting: workers().filter((worker) => /waiting/i.test(worker.status)).length,
    failed: workers().filter((worker) => /failed/i.test(worker.status)).length,
  }));
  const shown = computed(() => workers().length > 0);
  const summary = () => `${counts().total} workers, ${counts().active} active${counts().waiting ? `, ${counts().waiting} waiting for input` : ""}${counts().failed ? `, ${counts().failed} failed` : ""}`;
  const text = (size = 11, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  const button = (label: string, act: () => void, disabled = false) => <View role="button" accessibilityLabel={label}
    accessibilityState={{ disabled }} onPress={() => { if (!disabled) act(); }}
    style={{ minHeight: 27 * theme().scale, paddingHorizontal: 8, paddingVertical: 4, justifyContent: "center", borderRadius: 5, opacity: disabled ? 0.4 : 1 }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
    <Text style={text(11, theme().palette.text)}>{label}</Text>
  </View>;
  async function act(worker: ChatWorker, action: "send" | "stop") {
    if (!worker.sessionId || busy()) return;
    const sent = message();
    if (action === "send" && !sent.trim()) return;
    busy.set(worker.id);
    try {
      await props.handle.workerAction(worker.sessionId, action, action === "send" ? sent : undefined);
      if (action === "send" && message() === sent) { message.set(""); messaging.set(null); }
      stopping.set(null); refresh();
    } catch (error) { props.onError(`Worker ${action} failed: ${error}`); }
    finally { busy.set(null); }
  }
  return <View>{shown() && <View style={{ ...chatColumn(props.width?.() ?? 0, theme().scale), marginVertical: 5 * theme().scale }}>
    <View style={{ borderWidth: 1, borderColor: chatHairline(theme()), borderRadius: 8, backgroundColor: theme().palette.panel, overflow: "hidden" }}>
    <View role="button" accessibilityLabel={summary()} accessibilityState={{ expanded: expanded() }}
      onPress={() => expanded.set(!expanded())}
      style={{ flexDirection: "row", alignItems: "center", gap: 8 * theme().scale, paddingHorizontal: 10 * theme().scale, paddingVertical: 7 * theme().scale, minHeight: 34 * theme().scale }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Icon name={expanded() ? "down" : "right"} size={10 * theme().scale} color={theme().palette.muted} />
      <Icon name="person" size={13 * theme().scale} color={counts().active ? theme().palette.working : theme().palette.muted} />
      <Text style={{ ...text(12), fontWeight: "500", flexShrink: 0 }}>Workers</Text>
      <View style={{ flex: 1, flexDirection: "row", flexWrap: "wrap", columnGap: 8 * theme().scale, rowGap: 3 * theme().scale, alignItems: "center" }}>
        <Text style={text(11, theme().palette.muted)}>{`${counts().active} active · ${counts().total} total`}</Text>
        {!!counts().waiting && <Text style={text(11, theme().palette.gold)}>{`${counts().waiting} waiting`}</Text>}
        {!!counts().failed && <Text style={text(11, "#ef6b73")}>{`${counts().failed} failed`}</Text>}
        {!!busy() && <Text style={text(11, theme().palette.muted)}>Updating…</Text>}
      </View>
    </View>
    {expanded() && <>
    <View style={{ flexDirection: "row", alignItems: "center", paddingHorizontal: 7 * theme().scale, paddingVertical: 3, borderTopWidth: 1, borderColor: chatHairline(theme()) }}>
      <Text style={{ ...text(10, theme().palette.muted), flex: 1, paddingLeft: 3 * theme().scale }}>Select a worker for details and actions</Text>
      {button("Refresh", () => refresh())}
    </View>
    <Scroll style={{ maxHeight: 260 * theme().scale }}>
      {workers().map((worker) => <View style={{ padding: 10 * theme().scale, gap: 5 * theme().scale, borderTopWidth: 1, borderColor: chatHairline(theme()) }}>
        <View role="button" accessibilityLabel={worker.title} accessibilityState={{ expanded: selected() === worker.id }}
          onPress={() => selected.set(selected() === worker.id ? null : worker.id)} style={{ flexDirection: "row", alignItems: "center", minHeight: 25 * theme().scale, gap: 7 }}>
          <Icon name={selected() === worker.id ? "down" : "right"} size={10 * theme().scale} color={theme().palette.muted} />
          <Text numberOfLines={1} style={{ ...text(12), flex: 1 }}>{worker.title}</Text>
          <Text numberOfLines={2} style={{ ...text(11, /failed/i.test(worker.status) ? "#ef6b73" : /waiting/i.test(worker.status) ? theme().palette.gold : worker.active ? theme().palette.working : theme().palette.muted), maxWidth: "45%" }}>{worker.status}</Text>
        </View>
        <Text numberOfLines={selected() === worker.id ? 3 : 1} style={text(11, theme().palette.muted)}>{worker.assignment || worker.output.split("\n").filter(Boolean).slice(-1).join("") || "Waiting for worker output…"}</Text>
        {selected() === worker.id && <>
          {worker.model && <Text style={text(11, theme().palette.muted)}>{worker.model}</Text>}
          {worker.detail && <Text style={text(11, theme().palette.gold)}>{worker.detail}</Text>}
          {worker.output && <Scroll style={{ maxHeight: 130, padding: 7, backgroundColor: theme().palette.bg, borderRadius: 4 }}>
            <Text selectable={true} style={{ ...text(11), fontFamily: theme().monoFamily, lineHeight: 17 * theme().scale }}>{worker.output}</Text>
          </Scroll>}
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 3 }}>
            {worker.sessionId && (worker.kind === "shell" || worker.kind === "chat") && props.onOpen && button("Open", () => props.onOpen!(worker.sessionId!, worker.kind as "shell" | "chat"))}
            {button("Show in conversation", () => props.onReveal(worker.itemId))}
            {worker.canMessage && button("Message", () => { messaging.set(messaging() === worker.id ? null : worker.id); stopping.set(null); })}
            {worker.canStop && button("Stop", () => { stopping.set(stopping() === worker.id ? null : worker.id); messaging.set(null); }, !!busy())}
          </View>
          {messaging() === worker.id && <View style={{ flexDirection: "row", gap: 5 }}>
            <Input ref={(node) => node.focus()} value={message()} onChange={(value) => message.set(value)} placeholder="Message this worker" multiline={true} submitBehavior="submit"
              onSubmit={() => void act(worker, "send")} style={{ ...text(11), flex: 1, padding: 5, borderRadius: 4, borderWidth: 1, borderColor: theme().palette.divider }} />
            {button("Send", () => void act(worker, "send"), !!busy() || !message().trim())}
          </View>}
          {stopping() === worker.id && <View style={{ flexDirection: "row", alignItems: "center" }}>
            <Text style={{ ...text(10, theme().palette.gold), flex: 1 }}>End this worker’s shell process?</Text>
            {button("Cancel", () => stopping.set(null))}{button("Stop worker", () => void act(worker, "stop"), !!busy())}
          </View>}
        </>}
      </View>)}
    </Scroll>
    </>}
    </View>
  </View>}</View>;
}
