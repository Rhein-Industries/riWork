import { Button, Input } from "nsh/base";
import { Scroll, Text, View, cleanup, effect, host, signal, untracked, type Point } from "nsh";
import { Previews } from "riwork/native";
import { PreviewSurface } from "./PreviewSurface";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";

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
  cleanup(() => { alive = false; focusChanged(false); if (timer !== undefined) clearTimeout(timer); preview.close(); });
  effect(() => {
    const visible = effectiveVisible();
    if (!visible) untracked(() => focusChanged(false));
    void preview.setVisible(visible).catch(fail);
  });
  async function poll() {
    try { const next = await preview.snapshot(); if (alive && !busy()) state.set(next as PreviewState); }
    catch (reason) { fail(reason); }
    finally { if (alive) timer = setTimeout(() => { void poll(); }, 100); }
  }
  void poll();
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
  const active = () => !!state().pid || ["starting", "waitingforwindow", "streaming", "paused"].includes(state().phase);
  return <View testID="app-preview" style={{ flex: 1, minHeight: 0, gap: 8, padding: 10 }}>
    <View style={{ flexDirection: "row", gap: 10 }}>{(["cooperative", "window_capture", "ios_simulator"] as const).map((mode) =>
      <Button testID={`preview-mode:${mode}`} disabled={busy() || active()} onPress={() => { update({ mode }); if (mode === "ios_simulator" && !spec().ios) ios({}); commit(); }}>
        <Text style={{ ...style(), color: mode === spec().mode ? theme().palette.cyan : theme().palette.muted }}>{mode === "cooperative" ? "Hidden GPUI" : mode === "window_capture" ? "Cua window" : "iOS Simulator"}</Text>
      </Button>)}</View>
    <Input testID="preview-command" value={spec().command} placeholder="Command that starts your development app" readonly={busy() || active() || spec().mode === "ios_simulator"}
      onChange={(command) => update({ command })} onBlur={commit} onSubmit={commit} style={style()} />
    <Input testID="preview-cwd" value={spec().cwd} placeholder="Working folder" readonly={busy() || active()} onChange={(cwd) => update({ cwd })} onBlur={commit} onSubmit={commit} style={style()} />
    {spec().mode === "ios_simulator" && <View style={{ gap: 6 }}>{([ ["project", "Xcode project or workspace"], ["scheme", "Scheme"], ["device_type", "Device type (optional)"], ["runtime", "iOS runtime (optional)"] ] as const).map(([field, label]) =>
      <Input testID={`preview-ios:${field}`} value={spec().ios?.[field] ?? ""} placeholder={label} readonly={busy() || active()} onChange={(value) => ios({ [field]: field === "scheme" ? value : value || null } as Partial<NonNullable<PreviewSpec["ios"]>>)} onBlur={commit} onSubmit={commit} style={style()} />)}</View>}
    <View style={{ flexDirection: "row", gap: 12 }}>
      <Button testID="preview-run" disabled={busy()} onPress={() => { void run(); }}><Text style={style()}>{busy() ? "Starting…" : active() ? "Restart" : "Run"}</Text></Button>
      <Button testID="preview-stop" disabled={busy() || !active()} onPress={() => { void preview.stop().catch(fail); }}><Text style={style()}>Stop</Text></Button>
      {spec().mode !== "cooperative" && <Button testID="preview-show-window" disabled={!active()} onPress={() => input({ kind: "show_window" })}><Text style={style()}>Show window</Text></Button>}
      <Text testID="preview-phase" style={style()}>{state().phase}</Text>
    </View>
    <Text testID="preview-message" style={{ ...style(), color: error() ? theme().palette.gold : theme().palette.muted }}>{error() || state().warning || state().message}</Text>
    {state().windows.length > 1 && <View style={{ flexDirection: "row", gap: 8 }}>{state().windows.map((window) => <Button onPress={() => { void preview.selectWindow(window.pid, window.id).catch(fail); }}><Text style={style()}>{window.title || `Window ${window.id}`}</Text></Button>)}</View>}
    <PreviewSurface testID="preview-frame" owner={preview.surfaceId()} visible={effectiveVisible()} onFocusChange={(event) => focusChanged(event.focused)} style={{ flex: 1, minHeight: 80 }} />
    <Button testID="preview-logs-toggle" onPress={() => logsOpen.set(!logsOpen())}><Text style={style()}>{`${logsOpen() ? "▾" : "▸"} Logs (${state().logs.length})`}</Text></Button>
    {logsOpen() && <Scroll testID="preview-logs" style={{ height: 160, maxHeight: 180 }} contentStyle={{ gap: 2 }}>{state().logs.map((log) => <Text selectable={true} style={{ ...style(), fontFamily: theme().monoFamily, color: log.stderr ? theme().palette.gold : theme().palette.muted }}>{log.text}</Text>)}</Scroll>}
  </View>;
}
