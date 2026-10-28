import { Canvas, Path2D, Text, View, cleanup, effect, keymap, openUrl, signal } from "nsh";
import * as Native from "riwork/native";
import type { InterfaceTheme } from "../appearance";

export interface DictationSources { names: string[]; paths: string[]; text: string }
export interface DictationState {
  session: number;
  phase: "idle" | "preparing" | "listening" | "finishing" | "failed";
  text: string;
  note?: string | null;
  problem?: string | null;
  settingsUrl?: string | null;
  delivery?: { kind: "commit" | "discard"; text?: string } | null;
}
interface NativeDictationHandle {
  subscribe(listener: (state: DictationState) => void): { close(): void };
  start(sources: DictationSources): Promise<void>;
  stop(): Promise<void>;
  cancel(): Promise<void>;
  dismiss(): Promise<void>;
  close(): void;
}
// The host registry supplies these; keeping the shape local does not require type generation.
const Dictation = (Native as unknown as { Dictation: { open(): NativeDictationHandle } }).Dictation;

export interface DictationController {
  active(): boolean;
  status(): DictationState;
  userEdited(): void;
  stop(): void;
  cancel(): void;
  dismiss(): void;
  /** True means final words are settling and this callback will send them once. */
  finishBeforeSend(send: () => void): boolean;
}

interface DictationProps {
  theme: () => InterfaceTheme;
  enabled: boolean;
  active: boolean;
  disabled?: boolean;
  value: () => string;
  selection: () => { start: number; end: number };
  focused?: () => boolean;
  setValue: (text: string, start: number, end: number) => void;
  focus: () => void;
  sources?: () => DictationSources;
  onController: (controller: DictationController) => void;
}

/** The original insertion rules, using the NSH editor's UTF-16 offsets. Speech is a
 * replaceable span; edits outside it are rebased and edits inside it become user text. */
class Insertion {
  private base?: { text: string; start: number; end: number };
  private written?: string;
  private span?: { text: string; start: number; end: number };
  private heard = "";
  private claimed = 0;

  userEdited() { this.written = undefined; }
  reset() { this.base = undefined; this.written = undefined; this.span = undefined; this.heard = ""; this.claimed = 0; }

  show(text: string, selection: { start: number; end: number }, heard: string, focused = true) {
    if (!this.base || this.written !== text) {
      let rebased: { start: number; end: number } | undefined;
      if (this.span) {
        const previous = this.span.text;
        let prefix = 0;
        while (prefix < Math.min(previous.length, text.length) && previous.codePointAt(prefix) === text.codePointAt(prefix)) {
          prefix += previous.codePointAt(prefix)! > 0xffff ? 2 : 1;
        }
        let suffix = 0;
        const before = Array.from(previous.slice(prefix)).reverse();
        const after = Array.from(text.slice(prefix)).reverse();
        for (let at = 0; at < Math.min(before.length, after.length) && before[at] === after[at]; at++) suffix += before[at].length;
        const oldEnd = previous.length - suffix;
        const newEnd = text.length - suffix;
        if (prefix >= this.span.end) rebased = this.span;
        else if (oldEnd <= this.span.start) rebased = { start: this.span.start - oldEnd + newEnd, end: this.span.end - oldEnd + newEnd };
      }
      if (rebased) this.base = { text: text.slice(0, rebased.start) + text.slice(rebased.end), start: rebased.start, end: rebased.start };
      else {
        if (this.span) this.claimed = this.heard.match(/\S+/gu)?.length ?? 0;
        const anchor = !focused && selection.end === 0 && text.length ? { start: text.length, end: text.length } : selection;
        this.base = { text, start: anchor.start, end: anchor.end };
      }
    }
    const base = this.base;
    const words = [...heard.matchAll(/\S+/gu)];
    let piece = (words[this.claimed] ? heard.slice(words[this.claimed].index) : "").trim();
    const start = Math.max(0, Math.min(base.start, base.text.length));
    const end = Math.max(start, Math.min(base.end, base.text.length));
    let next = base.text;
    let caret = end;
    if (piece) {
      const before = Array.from(base.text.slice(0, start)).pop();
      const after = Array.from(base.text.slice(end))[0];
      if (before && !/\s/u.test(before) && !"([{\"'`".includes(before)) piece = ` ${piece}`;
      if (after && !/\s/u.test(after) && !".,;:!?)]}".includes(after)) piece += " ";
      next = base.text.slice(0, start) + piece + base.text.slice(end);
      caret = start + piece.length;
    }
    this.written = next;
    this.span = { text: next, start: piece ? start : caret, end: caret };
    this.heard = heard;
    return { text: next, start: caret, end: caret };
  }

  discard(text: string, selection: { start: number; end: number }) {
    const edit = this.base ? this.show(text, selection, "") : { text, ...selection };
    this.reset();
    return edit;
  }
}

const idle = (): DictationState => ({ session: 0, phase: "idle", text: "" });
const isActive = (state: DictationState) => ["preparing", "listening", "finishing"].includes(state.phase);

export function DictationControl(props: DictationProps) {
  const handle = Dictation.open();
  const state = signal<DictationState>(idle());
  const insertion = new Insertion();
  let alive = true;
  let minimumSession = 0;
  let completedSession = 0;
  let suppress = false;
  let deferredSend: (() => void) | undefined;
  let commands = Promise.resolve();

  function apply(edit: { text: string; start: number; end: number }) {
    props.setValue(edit.text, edit.start, edit.end);
  }
  function discard() { apply(insertion.discard(props.value(), props.selection())); }
  function fail(error: unknown) {
    if (!alive) return;
    deferredSend = undefined;
    // Infrastructure failures keep every word already shown.
    insertion.reset();
    state.set({ ...state(), phase: "failed", problem: String(error), settingsUrl: null });
  }
  function command(work: () => Promise<void>) {
    commands = commands.then(() => { if (alive) return work(); }).catch(fail);
  }
  function receive(next: DictationState) {
    if (!alive || next.session < minimumSession) return;
    if (suppress && isActive(next)) return;
    const previous = state();
    state.set(next);
    if (isActive(next)) {
      if (next.text !== previous.text || next.session !== previous.session && next.text) {
        apply(insertion.show(props.value(), props.selection(), next.text, props.focused?.() ?? true));
      }
      return;
    }
    if (next.delivery && completedSession !== next.session) {
      completedSession = next.session;
      if (next.delivery.kind === "commit" && !suppress) {
        apply(insertion.show(props.value(), props.selection(), next.delivery.text ?? "", props.focused?.() ?? true));
        insertion.reset();
      } else discard();
    }
    if (next.phase === "failed") deferredSend = undefined;
    if (deferredSend) {
      const send = deferredSend;
      deferredSend = undefined;
      // Let the controlled editor's draft generation settle before capturing the send.
      void Promise.resolve().then(() => { if (alive) send(); });
    }
  }
  const subscription = handle.subscribe(receive);

  function cancel() {
    suppress = true;
    deferredSend = undefined;
    discard();
    state.set({ ...state(), phase: "idle", text: "", problem: null, settingsUrl: null });
    command(() => handle.cancel());
  }
  function stop() {
    deferredSend = undefined;
    if (isActive(state())) command(() => handle.stop());
  }
  function toggle() {
    if (!props.enabled || props.disabled || !props.active) return;
    if (isActive(state())) { stop(); return; }
    insertion.reset();
    suppress = false;
    minimumSession = state().session + 1;
    state.set({ ...state(), phase: "preparing", text: "", note: null, problem: null, delivery: null });
    props.focus();
    const sources = props.sources?.() ?? { names: [], paths: [], text: "" };
    command(() => handle.start(sources));
  }
  const controller: DictationController = {
    active: () => isActive(state()), status: state,
    userEdited: () => insertion.userEdited(), stop, cancel,
    dismiss: () => { state.set({ ...state(), phase: "idle", problem: null, settingsUrl: null }); command(() => handle.dismiss()); },
    finishBeforeSend(send) {
      if (!isActive(state())) return false;
      deferredSend = send;
      command(() => handle.stop());
      return true;
    },
  };
  props.onController(controller);
  effect(() => {
    if (props.enabled) return;
    if (isActive(state())) cancel();
    else if (state().phase === "failed") controller.dismiss();
  });
  effect(() => {
    if (!props.enabled || !props.active || props.disabled) return;
    return keymap.bind("ctrl+alt+d", toggle, { when: "always" });
  });
  effect(() => {
    if (!props.active || !isActive(state())) return;
    return keymap.bind("escape", stop, { when: "always" });
  });
  cleanup(() => {
    alive = false;
    deferredSend = undefined;
    discard();
    subscription.close();
    handle.close();
  });

  const label = () => state().phase === "preparing" ? state().note ?? "Getting the microphone ready…"
    : state().phase === "finishing" ? "Finishing what was heard…"
    : state().phase === "listening" ? "Stop dictating and keep words · ⌃⌥D or ⎋ · ⏎ sends"
    : state().phase === "failed" ? "Dictate again · ⌃⌥D" : "Dictate · ⌃⌥D";
  return <View style={{ display: props.enabled ? "flex" : "none" }}>
    <View role="button" testID="chat-dictate" tooltip={label()} accessibilityLabel={label()}
      accessibilityState={{ disabled: !!props.disabled, selected: isActive(state()) }} onPress={toggle}
      style={{ width: 26 * props.theme().scale, height: 26 * props.theme().scale, borderRadius: 13 * props.theme().scale,
        alignItems: "center", justifyContent: "center", opacity: props.disabled ? 0.4 : 1,
        backgroundColor: isActive(state()) ? props.theme().palette.panelActive : "transparent" }}
      hoverStyle={{ backgroundColor: props.theme().palette.panelActive }}>
      <Canvas pointerEvents="none" style={{ width: 15 * props.theme().scale, height: 15 * props.theme().scale }} draw={(cx, size) => {
        cx.scale(size.width / 24, size.height / 24);
        cx.strokeStyle = isActive(state()) ? props.theme().palette.working : props.theme().palette.muted;
        cx.fillStyle = cx.strokeStyle;
        cx.lineWidth = 1.7;
        cx.lineCap = "round";
        cx.lineJoin = "round";
        const mic = new Path2D("M9 6a3 3 0 0 1 6 0v6a3 3 0 0 1-6 0Z");
        if (isActive(state())) cx.fill(mic); else cx.stroke(mic);
        cx.stroke(new Path2D("M5 11v1a7 7 0 0 0 14 0v-1M12 19v3M8 22h8"));
        if (state().phase === "failed") cx.stroke(new Path2D("M3 3l18 18"));
      }} />
    </View>
  </View>;
}

/** Companion row above the field; the microphone itself stays a compact toolbar symbol. */
export function DictationStatus(props: { theme: () => InterfaceTheme; controller: () => DictationController | undefined }) {
  const state = () => props.controller()?.status();
  const shown = () => state() && state()!.phase !== "idle";
  const failed = () => state()?.phase === "failed";
  const message = () => failed() ? state()?.problem : state()?.phase === "preparing" ? state()?.note ?? "Preparing microphone…"
    : state()?.phase === "finishing" ? "Finishing… · Enter sends" : "Listening · Enter sends";
  const text = (color: string) => ({ fontFamily: props.theme().family, fontSize: 11 * props.theme().scale, color });
  const button = (label: string, action: () => void) => <View role="button" accessibilityLabel={label} onPress={action}
    style={{ paddingHorizontal: 5, paddingVertical: 3, borderRadius: 4 }} hoverStyle={{ backgroundColor: props.theme().palette.panelActive }}>
    <Text style={text(props.theme().palette.muted)}>{label}</Text>
  </View>;
  return <View style={{ display: shown() ? "flex" : "none", flexDirection: "row", alignItems: "center", gap: 5, paddingVertical: 3 }}>
    <Text numberOfLines={3} style={{ ...text(failed() ? props.theme().palette.gold : props.theme().palette.working), flex: 1 }}>{message()}</Text>
    {failed() && state()?.settingsUrl && button("Open System Settings", () => {
      void openUrl(state()!.settingsUrl!).then(() => props.controller()?.dismiss());
    })}
    {failed() ? button("Dismiss", () => props.controller()?.dismiss()) : button("Cancel", () => props.controller()?.cancel())}
  </View>;
}
