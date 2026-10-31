import { Text, View, cleanup, effect, keymap, signal, untracked, type YNode } from "nsh";
import { Dashboard, type ChatApproval, type ChatDecision, type ChatQuestion } from "riwork/native";
import { Button, Input, settingsFocusScope } from "../SettingsFocus";
import { space, type InterfaceTheme } from "../appearance";
import type { DecisionRow } from "./model";

export interface Review {
  reviewId: string;
  chatId: string;
  requestId: string;
  title: string;
  kind: "approval" | "question";
  approval?: ChatApproval;
  question?: ChatQuestion;
  status: "pending" | "sending" | "sent" | "uncertain" | "unavailable";
  error?: string;
}

/** The two native calls a review makes; a fixture passes its own. */
export interface DecisionApi {
  reviewDecision(chatId: string, requestId: string, kind?: "approval" | "question"): Promise<Review>;
  respondDecision(reviewId: string, decision?: ChatDecision | null, answers?: string[][] | null): Promise<Review>;
}
const native = Dashboard as typeof Dashboard & DecisionApi;

/** Review a fresh provider request. The native review token retains the exact
 * request; sending rechecks it against the live host and never starts a chat.
 * The dashboard keeps this while the decision's row is unfolded, so a redraw of
 * the row (a new read, a region or width change) keeps its answers and status. */
export function createDecisionReview(decision: DecisionRow, options: { api?: DecisionApi; onResponded: () => void }) {
  const api = options.api ?? native;
  const review = signal<Review | null>(null);
  const loading = signal(true);
  const sending = signal(false);
  const uncertain = signal(false);
  const error = signal("");
  const picked = signal<string[][]>([]);
  const typed = signal<string[]>([]);
  let alive = true, focus = true;
  const busy = () => sending();
  const editable = () => !loading() && !busy() && !uncertain() && review()?.status === "pending";

  async function read() {
    if (busy() || uncertain()) return;
    loading.set(true);
    error.set("");
    try {
      const next = await api.reviewDecision(decision.chat_id, decision.request_id, decision.kind);
      if (!alive) return;
      review.set(next);
      picked.set(next.question?.prompts.map(() => []) ?? []);
      typed.set(next.question?.prompts.map(() => "") ?? []);
    } catch (cause) {
      if (alive) { review.set(null); error.set(`Could not read the request: ${String(cause)}`); }
    } finally { if (alive) loading.set(false); }
  }
  void read();

  function toggle(index: number, label: string, multi: boolean) {
    if (!editable()) return;
    const next = picked().map((labels) => [...labels]);
    const current = next[index] ?? [];
    next[index] = multi ? current.includes(label) ? current.filter((value) => value !== label) : [...current, label] : [label];
    picked.set(next);
    if (!multi) { const values = [...typed()]; values[index] = ""; typed.set(values); }
  }
  const selected = (index: number, label: string, multi: boolean) => (picked()[index] ?? []).includes(label) && (multi || !(typed()[index] ?? "").trim());
  function type(index: number, value: string) { if (editable()) { const next = [...typed()]; next[index] = value; typed.set(next); } }

  async function respond(choice?: ChatDecision) {
    const current = review();
    if (!current || !editable()) return;
    const answers = current.question?.prompts.map((prompt, index) => {
      const value = (typed()[index] ?? "").trim();
      // A free-form answer replaces a single choice; it adds to a multi-select.
      const values = prompt.multiSelect || !value ? [...(picked()[index] ?? [])] : [];
      if (value) values.push(value);
      return values;
    });
    if (current.kind === "question" && (!answers?.length || answers.some((values) => !values.length))) {
      error.set("Answer every question before sending.");
      return;
    }
    sending.set(true);
    error.set("");
    try {
      const next = await api.respondDecision(current.reviewId, choice ?? null, answers ?? null);
      if (!alive) return;
      review.set(next);
      if (next.status === "sent" || next.status === "uncertain" || next.status === "unavailable") options.onResponded();
    } catch (cause) {
      // A missing response is not evidence that the host rejected the command.
      // Keep the answer visible and prohibit another send from this review.
      if (alive) {
        uncertain.set(true);
        error.set(`Delivery could not be confirmed: ${String(cause)}. Open the chat to see whether the provider received it.`);
        options.onResponded();
      }
    } finally { if (alive) sending.set(false); }
  }

  const status = () => {
    if (loading()) return "Reading the current provider request…";
    if (busy()) return "Checking the live request and sending…";
    if (review()?.status === "sending") return "This response is being sent in another window. Read again to update its status, or open the chat.";
    if (uncertain() || review()?.status === "uncertain") return "Delivery is uncertain. This response cannot be sent again here; open the chat to check its state.";
    if (review()?.status === "sent") return "Sent. The inbox updates when the provider confirms the answer.";
    if (review()?.status === "unavailable") return "This request can no longer be answered from this review.";
    return "";
  };
  return {
    decision, review, loading, uncertain, error, typed, busy, editable, status, read, toggle, selected, type, respond,
    /** True once: the keyboard focus moves in when it unfolds, not on every redraw. */
    takeFocus: () => { const first = focus; focus = false; return first; },
    dispose: () => { alive = false; },
  };
}
export type DecisionReview = ReturnType<typeof createDecisionReview>;

/** The review unfolded under its decision's row, in the flow of the dashboard:
 * it covers nothing and holds no keys outside itself. Escape folds it while the
 * keyboard focus is in it (not while sending); the focus then goes back to `opener`. */
export function DecisionReviewView(props: {
  review: DecisionReview;
  theme: () => InterfaceTheme;
  opener: () => YNode | undefined;
  onClose: () => void;
  onOpenChat: () => void;
}) {
  const r = props.review;
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: props.theme().family, fontSize: value * props.theme().scale, color: muted ? palette().muted : palette().text });
  const actionStyle = () => ({ paddingHorizontal: size(10), paddingVertical: size(6), borderRadius: 5, borderWidth: 1, borderColor: palette().divider });
  const close = () => { if (!r.busy()) props.onClose(); };
  // Focus events do not bubble: each control of the review reports its own.
  const within = settingsFocusScope();
  function focusEvents() {
    const owner = {};
    cleanup(() => within.leave(owner));
    return { onFocus: () => within.enter(owner), onBlur: () => within.leave(owner) };
  }
  effect(() => { if (within.containsFocus()) return keymap.bind("escape", close, { when: "always" }); });
  // As the dialog did: the focus starts on Close, which answers nothing.
  let closeButton: YNode | undefined;
  effect(() => untracked(() => { if (r.takeFocus()) closeButton?.focus(); }));
  cleanup(() => { if (untracked(within.containsFocus)) queueMicrotask(() => props.opener()?.focus()); });

  return <View testID="dashboard-decision-review" style={{ gap: size(12), paddingHorizontal: size(8), paddingTop: size(4), paddingBottom: size(10) }}>
    {r.review()?.approval && <View style={{ gap: size(8) }}>
      <Text style={{ ...text(11), color: palette().gold }}>{r.review()!.approval!.heading}</Text>
      <Text selectable={true} style={{ ...text(12), fontFamily: r.review()!.approval!.kind === "command" ? props.theme().monoFamily : props.theme().family }}>{r.review()!.approval!.title}</Text>
      {!!r.review()!.approval!.detail && <Text selectable={true} style={text(11, true)}>{r.review()!.approval!.detail}</Text>}
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: size(8) }}>
        {r.review()!.approval!.choices.map((choice) => <Button testID={`dashboard-decision-${choice.decision}`} disabled={!r.editable()} onPress={() => void r.respond(choice.decision)} {...focusEvents()}
          style={{ ...actionStyle(), backgroundColor: choice.decision === "accept" || choice.decision === "accept_for_session" ? palette().panelActive : "transparent", opacity: r.editable() ? 1 : 0.5 }}
          focusStyle={{ borderColor: palette().cyan }}>
          <Text style={{ ...text(), color: choice.decision === "decline" || choice.decision === "cancel" ? palette().gold : palette().text }}>{choice.label}</Text>
        </Button>)}
      </View>
    </View>}
    {r.review()?.question && <View style={{ gap: size(14) }}>
      {r.review()!.question!.prompts.map((prompt, index) => <View style={{ gap: size(6) }}>
        {!!prompt.header && <Text style={text(10, true)}>{prompt.header}</Text>}
        <Text selectable={true} style={{ ...text(12), fontWeight: "600" }}>{prompt.question}</Text>
        {prompt.multiSelect && <Text style={text(10, true)}>Choose one or more, or type an answer.</Text>}
        {prompt.options.map((option) => <Button testID={`dashboard-answer-option-${index}-${option.label}`} selected={r.selected(index, option.label, prompt.multiSelect)} {...focusEvents()}
          accessibilityLabel={`${option.label}${option.description ? `. ${option.description}` : ""}`} disabled={!r.editable()} onPress={() => r.toggle(index, option.label, prompt.multiSelect)}
          style={{ ...actionStyle(), alignItems: "flex-start", gap: size(3), borderColor: r.selected(index, option.label, prompt.multiSelect) ? palette().cyan : palette().divider, opacity: r.editable() ? 1 : 0.5 }}
          focusStyle={{ backgroundColor: palette().panelActive }}>
          <Text style={text()}>{`${r.selected(index, option.label, prompt.multiSelect) ? "✓ " : ""}${option.label}`}</Text>
          {!!option.description && <Text style={text(10, true)}>{option.description}</Text>}
        </Button>)}
        <Input testID={`dashboard-answer-${index}`} accessibilityLabel={`Answer ${index + 1}: ${prompt.question}`} placeholder="Or type your answer" value={r.typed()[index] ?? ""} readonly={!r.editable()}
          onChange={(value) => r.type(index, value)} submitBehavior="submit" {...focusEvents()}
          style={{ ...text(), paddingHorizontal: size(8), paddingVertical: size(6), borderWidth: 1, borderRadius: 5, borderColor: palette().divider, backgroundColor: palette().bg }} />
      </View>)}
      <Button testID="dashboard-send-answer" disabled={!r.editable()} onPress={() => void r.respond()} {...focusEvents()} style={{ ...actionStyle(), alignSelf: "flex-start", backgroundColor: palette().panelActive, opacity: r.editable() ? 1 : 0.5 }} focusStyle={{ borderColor: palette().cyan }}>
        <Text style={text()}>Send answer</Text>
      </Button>
    </View>}
    {!!r.status() && <Text role="text" style={{ ...text(), color: r.review()?.status === "sent" ? palette().cyan : palette().muted }}>{r.status()}</Text>}
    {!!(r.error() || r.review()?.error) && <Text role="text" selectable={true} style={{ ...text(), color: palette().gold }}>{r.error() || r.review()?.error}</Text>}
    <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "flex-end", gap: size(8) }}>
      {!r.loading() && !r.uncertain() && (!r.review() || r.review()?.status === "unavailable" || r.review()?.status === "sending") && <Button testID="dashboard-review-again" disabled={r.busy()} onPress={() => void r.read()} {...focusEvents()} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Read again</Text></Button>}
      <Button testID="dashboard-review-open-chat" disabled={r.busy()} onPress={() => { if (!r.busy()) props.onOpenChat(); }} {...focusEvents()} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Open chat</Text></Button>
      <Button testID="dashboard-review-close" ref={(node) => { closeButton = node; }} disabled={r.busy()} onPress={close} {...focusEvents()} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Close</Text></Button>
    </View>
  </View>;
}
