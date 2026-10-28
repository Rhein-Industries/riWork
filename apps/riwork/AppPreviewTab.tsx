import { Button, Input } from "nsh/base";
import { Scroll, Text, View, cleanup, effect, host, signal, untracked, type Point } from "nsh";
import { Previews } from "riwork/native";
import { PreviewSurface } from "./PreviewSurface";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";

export interface PreviewSpec {
  command: string; cwd: string; mode: "cooperative" | "window_capture" | "ios_simulator";
  ios?: { project?: string | null; scheme: string; bundle_id?: string | null; device_type?: string | null; runtime?: string | null; configuration: string };
}
interface PreviewState {
  phase: string; message: string; warning?: string | null; pid?: number | null; windowId?: number | null;
  frame?: { width: number; height: number } | null;
  windows: { pid: number; id: number; title: string }[];
  logs: { text: string; stderr: boolean }[];
}
export function framePoint(point: Point, size: { width: number; height: number }, frame: { width: number; height: number }, clamp = false): Point | null {
  if ([point.x, point.y, size.width, size.height, frame.width, frame.height].some((value) => !Number.isFinite(value)) || frame.width <= 0 || frame.height <= 0) return null;
  const scale = Math.min(size.width / frame.width, size.height / frame.height);
  if (!Number.isFinite(scale) || scale <= 0) return null;
  const x = (point.x - (size.width - frame.width * scale) / 2) / scale;
  const y = (point.y - (size.height - frame.height * scale) / 2) / scale;
  if (!clamp && (x < 0 || y < 0 || x >= frame.width || y >= frame.height)) return null;
  return { x: Math.min(frame.width - 1, Math.max(0, x)), y: Math.min(frame.height - 1, Math.max(0, y)) };
}

export function previewViewport(width: number, height: number) {
  if (!Number.isFinite(width) || !Number.isFinite(height) || width < 32 || height < 32) return null;
  const scale = Math.min(1920 / width, 1200 / height, 1);
  return { width: Math.max(32, Math.round(width * scale)), height: Math.max(32, Math.round(height * scale)) };
}

/** One native owner per stable tab; configuration alone never starts it. */
export function AppPreviewTab(props: { spec: PreviewSpec; visible: boolean; theme?: () => InterfaceTheme; configure: (spec: PreviewSpec) => void; onFocusChange?: (focused: boolean) => void }) {
  const preview = Previews.open();
  const spec = signal<PreviewSpec>(untracked(() => ({ ...props.spec })));
  const state = signal<PreviewState>({ phase: "idle", message: "Enter a command, then select Run.", logs: [], windows: [] });
  const error = signal("");
  const busy = signal(false);
  const windowState = signal({ visible: true, appActive: true, occluded: false });
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const style = () => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text });
  let alive = true;
  let nativeFocused = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let pollRevision = 0;
  const logsOpen = signal(false);
  const effectiveVisible = () => props.visible && windowState().visible && windowState().appActive && !windowState().occluded;
  function focusChanged(focused: boolean) {
    const next = alive && effectiveVisible() && focused;
    if (nativeFocused === next) return;
    nativeFocused = next;
    props.onFocusChange?.(next);
  }
  const fail = (reason: unknown) => { if (alive) error.set(String(reason)); };
  cleanup(host.on("window.state", (next) => windowState.set({ ...windowState(), ...(next as object) })));
  cleanup(() => { alive = false; pollRevision++; focusChanged(false); if (timer !== undefined) clearTimeout(timer); preview.close(); });
  effect(() => {
    const visible = effectiveVisible();
    if (!visible) untracked(() => focusChanged(false));
    void preview.setVisible(visible).catch(fail);
    // Hidden tabs retain their native owner and last frame. Only the active
    // preview reads state; resuming immediately catches up with its owner.
    const revision = ++pollRevision;
    if (timer !== undefined) { clearTimeout(timer); timer = undefined; }
    if (visible) void poll(revision);
  });
  async function poll(revision: number) {
    if (!alive || revision !== pollRevision || !effectiveVisible()) return;
    try {
      const next = await preview.snapshot();
      if (alive && revision === pollRevision && !busy()) state.set(next as PreviewState);
    } catch (reason) { if (revision === pollRevision) fail(reason); }
    finally {
      if (alive && revision === pollRevision && effectiveVisible()) timer = setTimeout(() => { timer = undefined; void poll(revision); }, 100);
    }
  }
  function commit() { props.configure(spec()); }
  function update(change: Partial<PreviewSpec>) { spec.set({ ...spec(), ...change }); }
  function ios(change: Partial<NonNullable<PreviewSpec["ios"]>>) { update({ ios: { scheme: "", configuration: "Debug", ...spec().ios, ...change } }); }
  async function run() {
    if (busy()) return;
    commit(); busy.set(true); error.set("");
    state.set({ phase: "starting", message: "Starting preview…", logs: [], windows: [], frame: null, pid: null });
    try { await preview.run(spec()); } catch (reason) { fail(reason); } finally { if (alive) busy.set(false); }
  }
  function input(value: unknown) { void preview.input(value).catch(fail); }
  const active = () => ["starting", "waitingforwindow", "streaming", "paused"].includes(state().phase);
  const hasSession = () => state().phase !== "idle";
  const canStop = () => hasSession() && !["stopped", "exited"].includes(state().phase);
  const fieldStyle = () => ({ ...style(), minHeight: space(28, theme().scale), paddingHorizontal: space(8, theme().scale), paddingVertical: space(5, theme().scale), borderRadius: space(6, theme().scale), backgroundColor: theme().palette.panelActive });
  function button(id: string, label: string, onPress: () => void, disabled = false, selected = false) {
    return <Button testID={id} accessibilityLabel={label} disabled={disabled} selected={selected} onPress={onPress}
      style={{ paddingHorizontal: space(12, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: space(14, theme().scale), backgroundColor: selected && !disabled ? theme().palette.cyan : theme().palette.panelActive }}
      hoverStyle={{ backgroundColor: selected ? theme().palette.magenta : theme().palette.divider }} focusStyle={{ backgroundColor: theme().palette.divider }}>
      <Text style={{ ...style(), color: disabled ? theme().palette.muted : selected ? theme().palette.bg : theme().palette.text }}>{label}</Text>
    </Button>;
  }
  return <View testID="app-preview" style={{ flex: 1, minWidth: 0, minHeight: 0, backgroundColor: theme().palette.bg }}>
    <View style={{ padding: space(10, theme().scale), gap: space(6, theme().scale), borderBottomWidth: 1, borderBottomColor: theme().palette.divider, backgroundColor: theme().palette.panel }}>
      <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: space(6, theme().scale) }}>
        <Text style={{ ...style(), flex: 1, minWidth: space(90, theme().scale), fontWeight: "600" }}>App Preview</Text>
        {(["cooperative", "ios_simulator", "window_capture"] as const).map((mode) => button(`preview-mode:${mode}`, mode === "cooperative" ? "Hidden GPUI" : mode === "window_capture" ? "Cua window" : "iOS Simulator", () => {
          update({ mode }); if (mode === "ios_simulator" && !spec().ios) ios({}); commit();
        }, busy() || active(), mode === spec().mode))}
        {button("preview-run", busy() ? "Starting…" : "Run", () => void run(), busy() || active(), true)}
        {button("preview-restart", "Restart", () => void run(), busy() || !hasSession())}
        {button("preview-stop", "Stop", () => { void preview.stop().catch(fail); }, busy() || !canStop())}
        {spec().mode === "window_capture" && active() && button("preview-show-window", "Show window", () => input({ kind: "show_window" }), busy())}
      </View>
      {spec().mode !== "ios_simulator" ? <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space(6, theme().scale) }}>
        <Input testID="preview-command" accessibilityLabel="Command" value={spec().command} placeholder="Command that starts your development app" readonly={busy() || active()}
          onChange={(command) => update({ command })} onBlur={commit} onSubmit={commit} style={{ ...fieldStyle(), flex: 1, minWidth: space(160, theme().scale), fontFamily: theme().monoFamily }} />
        <Input testID="preview-cwd" accessibilityLabel="Working folder" value={spec().cwd} placeholder="Working folder" readonly={busy() || active()} onChange={(cwd) => update({ cwd })} onBlur={commit} onSubmit={commit} style={{ ...fieldStyle(), flex: 1, minWidth: space(160, theme().scale), fontFamily: theme().monoFamily }} />
      </View> : <>
        <Input testID="preview-cwd" accessibilityLabel="Working folder" value={spec().cwd} placeholder="Working folder" readonly={busy() || active()} onChange={(cwd) => update({ cwd })} onBlur={commit} onSubmit={commit} style={{ ...fieldStyle(), fontFamily: theme().monoFamily }} />
        {([[ ["scheme", "Scheme"], ["project", "Project / workspace"] ], [ ["device_type", "Device"], ["runtime", "iOS version"] ]] as const).map((pair) => <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space(6, theme().scale) }}>
          {pair.map(([field, label]) => <View style={{ flex: 1, minWidth: space(170, theme().scale), gap: 3 }}>
            <Text style={{ ...style(), color: theme().palette.muted }}>{label}</Text>
            <Input testID={`preview-ios:${field}`} accessibilityLabel={label} value={spec().ios?.[field] ?? ""} placeholder={field === "scheme" ? "App scheme" : `${label} (optional)`} readonly={busy() || active()}
              onChange={(value) => ios({ [field]: field === "scheme" ? value : value || null } as Partial<NonNullable<PreviewSpec["ios"]>>)} onBlur={commit} onSubmit={commit} style={fieldStyle()} />
          </View>)}
        </View>)}
      </>}
      <Text testID="preview-message" style={{ ...style(), fontSize: 10 * theme().scale, color: error() ? theme().palette.gold : theme().palette.muted }}>{error() || state().message}</Text>
      {state().warning && <Text style={{ ...style(), fontSize: 10 * theme().scale, color: theme().palette.gold }}>{state().warning}</Text>}
    </View>
    {state().phase === "waitingforwindow" && state().windows.length > 1 && <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 6, padding: 8 }}>
      <Text style={style()}>Choose an app window:</Text>
      {state().windows.map((window) => button(`preview-window:${window.id}`, window.title || `Window ${window.id}`, () => { void preview.selectWindow(window.pid, window.id).catch(fail); }))}
    </View>}
    <PreviewSurface testID="preview-frame" owner={preview.surfaceId()} visible={effectiveVisible()} onFocusChange={(event) => focusChanged(event.focused)} style={{ flex: 1, minHeight: 80 }} />
    <View style={{ borderTopWidth: 1, borderTopColor: theme().palette.divider, backgroundColor: theme().palette.panel, paddingHorizontal: space(10, theme().scale), paddingVertical: 4, flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 8 }}>
      {button("preview-logs-toggle", `${logsOpen() ? "▾" : "▸"} Logs (${state().logs.length})`, () => logsOpen.set(!logsOpen()))}
      <Text style={{ ...style(), flex: 1, minWidth: space(180, theme().scale), fontSize: 10 * theme().scale, color: theme().palette.muted }}>{spec().mode === "ios_simulator" ? "Tap and drag on the device to interact. The simulator belongs to this preview." : spec().mode === "cooperative" ? "Click the preview to type. Capture pauses while the tab or window is inactive." : "Input targets only the app launched here. RiWork Cua setup supplies capture and input permissions."}</Text>
    </View>
    {logsOpen() && <Scroll testID="preview-logs" style={{ height: space(160, theme().scale), maxHeight: space(180, theme().scale), minHeight: 0 }} contentStyle={{ padding: 8, gap: 2 }}>{state().logs.slice(-500).map((log) => <Text selectable={true} style={{ ...style(), fontSize: 10 * theme().scale, fontFamily: theme().monoFamily, color: log.stderr ? theme().palette.gold : theme().palette.muted }}>{log.text}</Text>)}</Scroll>}
  </View>;
}
