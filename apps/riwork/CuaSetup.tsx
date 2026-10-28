import { Button } from "nsh/base";
import { Text, View, cleanup, effect, signal, untracked } from "nsh";
import { Cua } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";

export interface CuaStatus {
  installed: boolean; version: string | null; running: boolean; ready: boolean;
  accessibility: boolean; screen_recording: boolean; direct_capture_verified: boolean;
  message: string;
  skills: { setup_needed: boolean; message: string; targets: { name: string; state: string; label: string }[] };
}
export interface CuaState { status: CuaStatus | null; pending: string | null; error: string | null }
export type CuaAction = "install" | "permissions" | "check";
export function cuaPresentation(state: CuaState) {
  const s = state.status;
  const link = !!s?.ready && s.skills.setup_needed;
  const repair = !!s?.installed && (!s.version || !s.running);
  const verify = !!s?.installed && s.accessibility && s.screen_recording && !s.direct_capture_verified;
  return {
    chip: state.pending ? "Working" : s?.ready ? "Driver ready" : "Setup needed",
    message: state.pending ?? state.error ?? s?.message ?? "Cua.ai controls desktop apps for every RiWork agent.",
    label: link ? "Link Cua skill" : s?.ready ? "Check Cua" : repair ? "Repair Cua" : verify ? "Verify screen capture" : s?.installed ? "Grant macOS access" : "Set up Cua",
    action: (link ? "install" : s?.ready ? "check" : !s?.version || !s.running ? "install" : s.installed ? "permissions" : "install") as CuaAction,
    help: !!s?.installed && !s.ready && !verify,
  };
}
export function useCua() {
  const state = signal<CuaState>({ status: null, pending: null, error: null });
  let alive = true;
  const subscription = Cua.subscribe((event) => state.set((event as { data: CuaState }).data));
  cleanup(() => { alive = false; subscription.close(); });
  async function run(action: CuaAction) {
    if (state().pending) return;
    state.set({ ...state(), error: null, pending: action === "install" ? "Setting up Cua.ai…" : action === "permissions" ? "Opening macOS access settings…" : "Checking Cua.ai…" });
    try { const next = await Cua.run(action); if (alive) state.set(next as CuaState); }
    catch (error) { if (alive) state.set({ ...state(), pending: null, error: String(error) }); }
  }
  return { state, run };
}

/** Independent Settings section: 6a can place this without owning Cua state. */
export function CuaSetup(props: { theme?: () => InterfaceTheme; controller?: ReturnType<typeof useCua>; visible?: boolean; checkOnShow?: boolean }) {
  const controller = props.controller ?? useCua();
  const state = controller.state;
  effect(() => { if (props.visible !== false && props.checkOnShow !== false) untracked(() => { void controller.run("check"); }); });
  const presentation = () => cuaPresentation(state());
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const style = () => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text });
  return <View testID="cua-setup" style={{ gap: 9, padding: 12 }}>
    <View style={{ flexDirection: "row", gap: 10 }}><Text style={style()}>Cua.ai</Text>
      <Text testID="cua-state" style={{ ...style(), color: state().status?.ready ? theme().palette.cyan : theme().palette.gold }}>{presentation().chip}</Text></View>
    <Text testID="cua-message" style={{ ...style(), color: state().error ? theme().palette.gold : theme().palette.muted }}>{presentation().message}</Text>
    {state().status?.installed && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 10 }}>
      <Text style={style()}>{`Accessibility ${state().status!.accessibility ? "ready" : "needed"}`}</Text>
      <Text style={style()}>{`Recording ${state().status!.screen_recording ? "ready" : "needed"}`}</Text>
      <Text style={style()}>{`Capture ${state().status!.direct_capture_verified ? "verified" : "unverified"}`}</Text>
    </View>}
    {presentation().help && <Text style={style()}>Enable CuaDriver in macOS Accessibility and Screen Recording to connect all agents.</Text>}
    <Text style={{ ...style(), color: theme().palette.muted }}>New agent sessions connect automatically. Restart existing sessions to connect them.</Text>
    {state().status && <View style={{ gap: 6 }}><Text style={style()}>cua-driver skill</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 10 }}>{state().status!.skills.targets.map((target) =>
        <Text style={{ ...style(), color: target.state === "linked" ? theme().palette.cyan : ["home_absent", "unsupported"].includes(target.state) ? theme().palette.muted : theme().palette.gold }}>{`${target.name} ${target.label}`}</Text>)}</View>
      <Text style={{ ...style(), color: theme().palette.muted }}>{state().status!.skills.message}</Text>
    </View>}
    <View style={{ flexDirection: "row", gap: 12 }}>
      <Button testID="cua-primary" disabled={!!state().pending} onPress={() => { void controller.run(presentation().action); }}><Text style={style()}>{presentation().label}</Text></Button>
      <Button testID="cua-check" disabled={!!state().pending} onPress={() => { void controller.run("check"); }}><Text style={style()}>Check again</Text></Button>
    </View>
  </View>;
}
