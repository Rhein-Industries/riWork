import { Text, View, signal } from "nsh";
import { Button, Input } from "nsh/base";
import type { FlowRunDto, FlowStepDto } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";

export function flowText(theme: () => InterfaceTheme) {
  return (size = 11, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
}

const STATUS: Record<string, string> = { active: "Working", waiting: "Waits for you", completed: "Completed", stopped: "Stopped" };

function clock(at: number) {
  const date = new Date(at * 1000);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** One line of a run's history, as the person reads it. */
export function stepLine(step: FlowStepDto): string {
  const evidence = step.evidence ? ` — “${step.evidence}”` : "";
  const note = step.detail ? ` — ${step.detail.split("\n")[0]}` : "";
  switch (step.kind) {
    case "start": return `Started in ${step.to}`;
    case "moved": return `${step.event}: ${step.from} → ${step.to}${evidence}`;
    case "asked": return `Asked to move on with ${step.event}${evidence}`;
    case "approved": return `You approved ${step.event}${note}`;
    case "denied": return `You turned down ${step.event}${note}`;
    case "check_failed": return `Check for ${step.event} failed${note}`;
    case "stopped": return "Stopped";
    default: return step.kind;
  }
}

/** A run: where its chat is, the decision it waits for, and its history. */
export function RunCard(props: {
  run: FlowRunDto;
  theme: () => InterfaceTheme;
  busy: () => boolean;
  shown?: boolean;
  /** The chat strip leaves out what the chat shows by itself. */
  compact?: boolean;
  onShow?: () => void;
  onOpenChat?: () => void;
  onApprove: (note: string) => void;
  onDeny: (note: string) => void;
  onStop: () => void;
}) {
  const theme = props.theme;
  const palette = () => theme().palette;
  const text = flowText(theme);
  const note = signal("");
  const expanded = signal(false);
  const run = () => props.run;
  const live = () => run().status === "active" || run().status === "waiting";
  const history = () => expanded() ? run().history : run().history.slice(-4);
  function button(id: string, label: string, onPress: () => void, primary = false) {
    return <Button testID={id} accessibilityLabel={label} disabled={props.busy()} onPress={onPress}
      style={{ paddingHorizontal: space(10, theme().scale), paddingVertical: space(3, theme().scale), borderRadius: space(12, theme().scale), backgroundColor: primary && !props.busy() ? palette().cyan : palette().panelActive }}
      hoverStyle={{ backgroundColor: primary ? palette().magenta : palette().divider }} focusStyle={{ backgroundColor: palette().divider }}>
      <Text style={text(11, props.busy() ? palette().muted : primary ? palette().bg : palette().text)}>{label}</Text>
    </Button>;
  }
  return <View testID={`flow-run:${run().id}`} style={{ gap: 6, padding: space(props.compact ? 8 : 12, theme().scale), borderRadius: space(props.compact ? 8 : 12, theme().scale), backgroundColor: props.compact ? palette().panel : palette().bg, borderWidth: props.shown ? 1 : 0, borderColor: palette().divider }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
      <Text numberOfLines={1} style={{ ...text(11), fontWeight: "600", flexShrink: 1 }}>{props.compact ? `Flow: ${run().flowName}` : run().chatTitle || `Chat ${run().chatId.slice(0, 8)}`}</Text>
      <Text style={text(11)}>{`· ${run().location}`}</Text>
      <Text style={{ ...text(10, run().status === "waiting" ? palette().gold : palette().muted), marginLeft: "auto" }}>{STATUS[run().status] ?? run().status}</Text>
    </View>
    {!!run().error && <Text selectable={true} style={text(10, palette().gold)}>{run().error!}</Text>}
    {run().status === "waiting" && <View testID="flow-decision" style={{ gap: 6 }}>
      <Text style={text(11)}>{`The agent asks to move on with ${run().pendingEvent} → ${run().pendingLocation}.`}</Text>
      {!!run().pendingEvidence && <Text selectable={true} style={text(11, palette().muted)}>{`Evidence: ${run().pendingEvidence}`}</Text>}
      <Input testID="flow-note" accessibilityLabel="Note for the agent" placeholder="Note for the agent (optional)" value={note()} onChange={(value) => note.set(value)}
        style={{ ...text(11), minHeight: space(26, theme().scale), backgroundColor: props.compact ? palette().bg : palette().panel, borderRadius: space(6, theme().scale), paddingHorizontal: space(8, theme().scale), paddingVertical: space(4, theme().scale) }} />
      <View style={{ flexDirection: "row", gap: 6, flexWrap: "wrap" }}>
        {button("flow-approve", "Approve", () => { props.onApprove(note()); note.set(""); }, true)}
        {button("flow-deny", "Not yet", () => { props.onDeny(note()); note.set(""); })}
      </View>
    </View>}
    {!props.compact && run().history.length > 0 && <View style={{ gap: 2 }}>
      {history().map((step) => <Text selectable={true} style={text(10, step.kind === "check_failed" ? palette().gold : palette().muted)}>{`${clock(step.at)}  ${stepLine(step)}`}</Text>)}
      {run().history.length > 4 && <View role="button" accessibilityLabel={expanded() ? "Show less" : "Show the whole history"} onPress={() => expanded.set(!expanded())}>
        <Text style={text(10, palette().cyan)}>{expanded() ? "Show less" : `Show all ${run().history.length} steps`}</Text>
      </View>}
    </View>}
    <View style={{ flexDirection: "row", gap: 6, flexWrap: "wrap" }}>
      {!!props.onShow && !props.shown && !!run().mermaid && button(`flow-show:${run().id}`, "Show on diagram", props.onShow)}
      {!!props.onOpenChat && button(`flow-open-chat:${run().id}`, "Open chat", props.onOpenChat)}
      {live() && button(`flow-stop:${run().id}`, "Stop flow", props.onStop)}
    </View>
  </View>;
}
