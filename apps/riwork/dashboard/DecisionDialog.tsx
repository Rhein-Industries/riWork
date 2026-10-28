import { Scroll, Text, View, cleanup, signal, type YNode } from "nsh";
import { Button, Dialog, Input } from "nsh/base";
import { Dashboard, type ChatApproval, type ChatDecision, type ChatQuestion } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";
import { sessionName, type DecisionRow } from "./model";

interface Review {
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

const decisions = Dashboard as typeof Dashboard & {
  reviewDecision(chatId: string, requestId: string, kind?: "approval" | "question"): Promise<Review>;
  respondDecision(reviewId: string, decision?: ChatDecision | null, answers?: string[][] | null): Promise<Review>;
};

/** Review a fresh provider request. The native review token retains the exact
 * request; sending rechecks it against the live host and never starts a chat. */
export function DecisionDialog(props: {
  decision: DecisionRow;
  theme: () => InterfaceTheme;
  onClose: () => void;
  onOpenChat: () => void;
  onResponded: () => void;
}) {
  const review = signal<Review | null>(null);
  const loading = signal(true);
  const sending = signal(false);
  const uncertain = signal(false);
  const error = signal("");
  const picked = signal<string[][]>([]);
  const typed = signal<string[]>([]);
  let alive = true;
  let closeButton: YNode | undefined;
  cleanup(() => { alive = false; });
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: props.theme().family, fontSize: value * props.theme().scale, color: muted ? palette().muted : palette().text });
  const busy = () => sending();
  const editable = () => !loading() && !busy() && !uncertain() && review()?.status === "pending";
  const actionStyle = () => ({ paddingHorizontal: size(10), paddingVertical: size(6), borderRadius: 5, borderWidth: 1, borderColor: palette().divider });
  const close = () => { if (!busy()) props.onClose(); };

  async function read() {
    if (busy() || uncertain()) return;
    loading.set(true);
    error.set("");
    try {
      const next = await decisions.reviewDecision(props.decision.chat_id, props.decision.request_id, props.decision.kind);
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

  async function respond(decision?: ChatDecision) {
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
      const next = await decisions.respondDecision(current.reviewId, decision ?? null, answers ?? null);
      if (!alive) return;
      review.set(next);
      if (next.status === "sent" || next.status === "uncertain" || next.status === "unavailable") props.onResponded();
    } catch (cause) {
      // A missing response is not evidence that the host rejected the command.
      // Keep the answer visible and prohibit another send from this dialog.
      if (alive) {
        uncertain.set(true);
        error.set(`Delivery could not be confirmed: ${String(cause)}. Open the chat to see whether the provider received it.`);
        props.onResponded();
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

  return <Dialog testID="dashboard-decision-review" animation="none" closeOnBackdropPress={false} closeOnEscape={!busy()}
    onOpen={() => closeButton?.focus()} onOpenChange={(open) => { if (!open) close(); }} backdropStyle={{ backgroundColor: "#00000066" }}
    style={{ width: size(620), maxWidth: "100%", maxHeight: "90%", padding: size(18), gap: size(12), borderRadius: 10, borderWidth: 1, borderColor: palette().divider, backgroundColor: palette().panel }}>
    <View style={{ gap: size(4) }}>
      <Text role="header" style={{ ...text(15), fontWeight: "600" }}>Review decision</Text>
      <Text style={text(10, true)}>{`${sessionName(props.decision.session)} · ${props.decision.session.project_name ?? "Global"}`}</Text>
    </View>
    <Scroll style={{ flexShrink: 1, minHeight: 0 }} contentStyle={{ gap: size(12) }}>
      {review()?.approval && <View style={{ gap: size(8) }}>
        <Text style={{ ...text(11), color: palette().gold }}>{review()!.approval!.heading}</Text>
        <Text selectable={true} style={{ ...text(12), fontFamily: review()!.approval!.kind === "command" ? props.theme().monoFamily : props.theme().family }}>{review()!.approval!.title}</Text>
        {!!review()!.approval!.detail && <Text selectable={true} style={text(11, true)}>{review()!.approval!.detail}</Text>}
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: size(8) }}>
          {review()!.approval!.choices.map((choice) => <Button testID={`dashboard-decision-${choice.decision}`} disabled={!editable()} onPress={() => void respond(choice.decision)}
            style={{ ...actionStyle(), backgroundColor: choice.decision === "accept" || choice.decision === "accept_for_session" ? palette().panelActive : "transparent", opacity: editable() ? 1 : 0.5 }}
            focusStyle={{ borderColor: palette().cyan }}>
            <Text style={{ ...text(), color: choice.decision === "decline" || choice.decision === "cancel" ? palette().gold : palette().text }}>{choice.label}</Text>
          </Button>)}
        </View>
      </View>}
      {review()?.question && <View style={{ gap: size(14) }}>
        {review()!.question!.prompts.map((prompt, index) => <View style={{ gap: size(6) }}>
          {!!prompt.header && <Text style={text(10, true)}>{prompt.header}</Text>}
          <Text selectable={true} style={{ ...text(12), fontWeight: "600" }}>{prompt.question}</Text>
          {prompt.multiSelect && <Text style={text(10, true)}>Choose one or more, or type an answer.</Text>}
          {prompt.options.map((option) => <Button testID={`dashboard-answer-option-${index}-${option.label}`} selected={selected(index, option.label, prompt.multiSelect)}
            accessibilityLabel={`${option.label}${option.description ? `. ${option.description}` : ""}`} disabled={!editable()} onPress={() => toggle(index, option.label, prompt.multiSelect)}
            style={{ ...actionStyle(), alignItems: "flex-start", gap: size(3), borderColor: selected(index, option.label, prompt.multiSelect) ? palette().cyan : palette().divider, opacity: editable() ? 1 : 0.5 }}
            focusStyle={{ backgroundColor: palette().panelActive }}>
            <Text style={text()}>{`${selected(index, option.label, prompt.multiSelect) ? "✓ " : ""}${option.label}`}</Text>
            {!!option.description && <Text style={text(10, true)}>{option.description}</Text>}
          </Button>)}
          <Input testID={`dashboard-answer-${index}`} accessibilityLabel={`Answer ${index + 1}: ${prompt.question}`} placeholder="Or type your answer" value={typed()[index] ?? ""} readonly={!editable()}
            onChange={(value) => { if (editable()) { const next = [...typed()]; next[index] = value; typed.set(next); } }}
            style={{ ...text(), paddingHorizontal: size(8), paddingVertical: size(6), borderWidth: 1, borderRadius: 5, borderColor: palette().divider }} />
        </View>)}
        <Button testID="dashboard-send-answer" disabled={!editable()} onPress={() => void respond()} style={{ ...actionStyle(), alignSelf: "flex-start", backgroundColor: palette().panelActive, opacity: editable() ? 1 : 0.5 }} focusStyle={{ borderColor: palette().cyan }}>
          <Text style={text()}>Send answer</Text>
        </Button>
      </View>}
      {!!status() && <Text role="text" style={{ ...text(), color: review()?.status === "sent" ? palette().cyan : palette().muted }}>{status()}</Text>}
      {!!(error() || review()?.error) && <Text role="text" selectable={true} style={{ ...text(), color: palette().gold }}>{error() || review()?.error}</Text>}
    </Scroll>
    <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "flex-end", gap: size(8) }}>
      {!loading() && !uncertain() && (!review() || review()?.status === "unavailable" || review()?.status === "sending") && <Button testID="dashboard-review-again" disabled={busy()} onPress={() => void read()} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Read again</Text></Button>}
      <Button testID="dashboard-review-open-chat" disabled={busy()} onPress={() => { if (!busy()) props.onOpenChat(); }} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Open chat</Text></Button>
      <Button testID="dashboard-review-close" ref={(node) => { closeButton = node; }} disabled={busy()} onPress={close} style={actionStyle()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Close</Text></Button>
    </View>
  </Dialog>;
}
