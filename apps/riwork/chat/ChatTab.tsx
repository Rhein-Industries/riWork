import { List, Scroll, Text, View, cleanup, computed, effect, keymap, signal, type YNode } from "nsh";
import { Input, TextView } from "nsh/base";
import { Menu, type MenuItem } from "nsh/desktop";
import {
  Chat, Settings, type ChatApproval, type ChatBody, type ChatDraft, type ChatHandle, type ChatPanel,
  type ChatQuestion, type ChatRow,
} from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import {
  APPROVAL_GUARD_MS, SEND_TIP, STEER_TIP, STOP_TIP, answersOf, applyRows, bodyChanges,
  decisionForKey, detailLines, emptyPanel, viewport, type Decision,
} from "./model";

const ERROR = "#ef6b73";

export interface ChatTabProps {
  chatId: string;
  visible: boolean;
  theme?: () => InterfaceTheme;
  onTitle?: (title: string) => void;
  onClose?: () => void;
  /** Headless fixtures: report cell constructions and state. */
  onDiagnostics?: (report: ChatDiagnostics) => void;
}

export interface ChatDiagnostics {
  rows: number;
  /** Row cells alive now (materialized), and ever constructed. */
  cells: number;
  constructed: number;
  bodies: number;
  groups: string[];
  panel: ChatPanel;
  /** The draft editor identities this tab's composers took, in order. */
  editors: number[];
}

/** A chat tab: the transcript List over the bridge's rows, the requests that
 * wait for the user, and the composer. Rows are headers; a row's body comes by
 * viewport and updates on its own signal, so streaming re-renders one cell. */
export function ChatTab(props: ChatTabProps) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const handle: ChatHandle = Chat.open(props.chatId);
  cleanup(() => handle.close());
  const rows = signal<ChatRow[]>([]);
  const panel = signal<ChatPanel>(emptyPanel(props.chatId));
  const notices = signal<string[]>([]);
  const bodies = new Map<string, ReturnType<typeof signal<ChatBody | undefined>>>();
  const bodySignal = (key: string) => {
    let body = bodies.get(key);
    if (!body) { body = signal<ChatBody | undefined>(undefined); bodies.set(key, body); }
    return body;
  };
  let cells = 0;
  let constructed = 0;
  const editors: number[] = [];
  let list: YNode | undefined;
  let following = true;
  const report = () => props.onDiagnostics?.({
    rows: rows().length, cells, constructed, bodies: [...bodies.values()].filter((body) => body() !== undefined).length,
    groups: rows().filter((row) => row.kind === "group" && !row.open).map((row) => row.id), panel: panel(),
    editors: [...editors],
  });
  const subscription = handle.subscribe((delivery) => {
    if (delivery.kind === "reset") for (const body of bodies.values()) body.set(undefined);
    for (const [key, body] of bodyChanges((key) => bodies.get(key)?.(), delivery)) bodySignal(key).set(body);
    const next = applyRows(rows(), delivery);
    if (next !== rows()) rows.set(next);
    if (delivery.panel) {
      panel.set(delivery.panel);
      props.onTitle?.(delivery.panel.title);
      if (delivery.panel.failures.length) notices.set([...notices(), ...delivery.panel.failures].slice(-3));
    }
    if (delivery.kind === "reset" && following) list?.scrollToEnd();
    report();
  });
  cleanup(() => subscription.close());

  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const mono = (base = 11, color = theme().palette.text) => ({ fontFamily: theme().monoFamily, fontSize: base * theme().scale, color });
  const notice = (message: string) => notices.set([...notices(), message].slice(-3));

  // ⌘. interrupts from anywhere in a visible chat, only while it runs (CHATT-020).
  effect(() => {
    if (!props.visible || !panel().running) return;
    return keymap.bind("mod+.", () => { void handle.interrupt().catch((error) => notice(`Could not reach the chat: ${error}`)); }, { when: "always" });
  });
  // ⌘⇧V switches Normal/Verbose; ⌘⌥→/← expand or collapse (CHATT-031).
  effect(() => {
    if (!props.visible) return;
    return keymap.bind({
      "mod+shift+v": () => { following = true; handle.setMode(panel().mode === "normal" ? "verbose" : "normal"); },
      "mod+alt+right": () => handle.expandAll(true),
      "mod+alt+left": () => handle.expandAll(false),
    });
  });

  function Row(rowProps: { row: () => ChatRow }) {
    cells += 1;
    constructed += 1;
    cleanup(() => { cells -= 1; });
    const row = rowProps.row;
    const indent = () => space(20, theme().scale) + row().depth * space(12, theme().scale);
    return <View testID={`chat-row:${row().key}`} style={{ paddingLeft: indent(), paddingRight: space(20, theme().scale), paddingVertical: 1, maxWidth: 1080 }}>
      {row().disclosure ? <Header row={row} /> : <Leaf row={row} />}
      {row().disclosure && row().open && row().hasBody && <Body row={row} />}
    </View>;
  }

  function Header(headerProps: { row: () => ChatRow }) {
    const row = headerProps.row;
    const label = () => [row().title, row().target, row().failedCount && !row().title.includes("failed") ? `· ${row().failedCount} failed` : ""].filter(Boolean).join(" ");
    return <View role="button" testID={`chat-header:${row().id}`} accessibilityLabel={label()} accessibilityState={{ expanded: row().open }}
      onPress={() => { following = false; handle.toggle(row().id); }}
      style={{ height: space(24, theme().scale), flexDirection: "row", alignItems: "center", gap: 6, borderRadius: 4,
        backgroundColor: row().kind === "group" ? theme().palette.panel : "transparent" }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text style={text(10, theme().palette.muted)}>{row().open ? "▾" : "▸"}</Text>
      <Text numberOfLines={1} style={{ ...text(12), flexShrink: 1 }}>{row().title}</Text>
      {row().target && <Text numberOfLines={1} style={{ ...mono(11), flexShrink: 2 }}>{row().target}</Text>}
      {row().meta && <Text numberOfLines={1} style={{ ...text(11, theme().palette.muted), flexShrink: 3 }}>{`· ${row().meta}`}</Text>}
      {row().running && <Text style={text(10, theme().palette.working)}>Running</Text>}
      {!row().running && row().failedCount > 0 && <Text style={text(10, ERROR)}>{`${row().failedCount} failed`}</Text>}
    </View>;
  }

  function Leaf(leafProps: { row: () => ChatRow }) {
    const row = leafProps.row;
    const kind = () => row().kind;
    return <>
      {kind() === "user" && <View style={{ marginVertical: 6, padding: 10, borderRadius: 12, backgroundColor: theme().palette.panelActive }}>
        <Body row={row} />
      </View>}
      {(kind() === "final" || kind() === "commentary" || kind() === "reasoning" || kind() === "notice") && <View style={{ marginVertical: 4 }}><Body row={row} /></View>}
      {kind() === "compaction" && <View style={{ flexDirection: "row", alignItems: "center", gap: 8, marginVertical: 6 }}>
        <View style={{ flex: 1, height: 1, backgroundColor: theme().palette.divider }} />
        <Text style={text(10, theme().palette.muted)}>{Settings.sentenceCase("context compacted")}</Text>
        <View style={{ flex: 1, height: 1, backgroundColor: theme().palette.divider }} />
      </View>}
      {kind() === "request" && <View style={{ height: space(24, theme().scale), justifyContent: "center" }}>
        <Text numberOfLines={1} style={text(11, theme().palette.muted)}>{`✓ ${row().title}${row().target ? ` · ${row().target}` : ""}`}</Text>
      </View>}
    </>;
  }

  function Body(bodyProps: { row: () => ChatRow }) {
    const body = () => bodySignal(bodyProps.row().key)();
    const muted = () => bodyProps.row().kind === "commentary";
    return <>
      {!body() && <Text style={text(11, theme().palette.muted)}>…</Text>}
      {body()?.kind === "markdown" && <TextView selectable={true} links="report" testID={`chat-body:${bodyProps.row().key}`}
        document={{ blocks: (body() as Extract<ChatBody, { kind: "markdown" }>).blocks }}
        styles={{ root: { ...text(muted() ? 12 : 13, muted() ? theme().palette.muted : theme().palette.text) }, code: mono(12), codeBlock: { backgroundColor: theme().palette.panel } }} />}
      {body()?.kind === "text" && <Text selectable={true} testID={`chat-body:${bodyProps.row().key}`} style={{
        ...text(12, toneColor((body() as Extract<ChatBody, { kind: "text" }>).tone)),
        fontStyle: (body() as Extract<ChatBody, { kind: "text" }>).tone === "reasoning" ? "italic" : "normal",
      }}>{(body() as Extract<ChatBody, { kind: "text" }>).text}</Text>}
      {body()?.kind === "command" && <CommandBox body={() => body() as Extract<ChatBody, { kind: "command" }>} />}
      {body()?.kind === "detail" && <Scroll style={{ maxHeight: 320, padding: 8, borderRadius: 4, backgroundColor: theme().palette.panel }}>
        <Text selectable={true} style={mono(11)}>{(body() as Extract<ChatBody, { kind: "detail" }>).text}</Text>
      </Scroll>}
      {body()?.kind === "steps" && <View style={{ gap: 2, paddingVertical: 4 }}>
        {(body() as Extract<ChatBody, { kind: "steps" }>).explanation && <Text style={text(12)}>{(body() as Extract<ChatBody, { kind: "steps" }>).explanation}</Text>}
        {(body() as Extract<ChatBody, { kind: "steps" }>).steps.map((step) => <Text style={text(12, step.status === "completed" ? theme().palette.muted : theme().palette.text)}>
          {`${step.status === "completed" ? "✓" : step.status === "in_progress" ? "◐" : "○"} ${step.text}`}
        </Text>)}
      </View>}
    </>;
  }

  function CommandBox(boxProps: { body: () => Extract<ChatBody, { kind: "command" }> }) {
    const body = boxProps.body;
    return <View style={{ gap: 2, paddingVertical: 2 }}>
      <Text numberOfLines={1} style={mono(11, theme().palette.muted)}>{`$ ${body().command.split("\n")[0]}${body().command.includes("\n") ? " …" : ""}${body().exitCode ? ` · exit ${body().exitCode}` : ""}`}</Text>
      <Scroll style={{ maxHeight: 320, padding: 8, borderRadius: 4, backgroundColor: theme().palette.panel }}>
        {body().omitted > 0 && <Text style={{ ...mono(10, theme().palette.muted), fontStyle: "italic" }}>{`${body().omitted} earlier bytes not shown`}</Text>}
        <Text selectable={true} style={mono(11)}>{body().output || (body().running ? "Waiting for output…" : "")}</Text>
      </Scroll>
    </View>;
  }

  function toneColor(tone: string): string {
    if (tone === "reasoning") return theme().palette.muted;
    if (tone === "warning") return theme().palette.gold;
    if (tone === "error") return ERROR;
    return theme().palette.text;
  }

  function Footer() {
    const status = () => panel().status;
    return <View testID="chat-footer" style={{ paddingHorizontal: space(20, theme().scale), paddingVertical: 10, gap: 4 }}>
      {status() && <Text testID="chat-status" style={text(11, panel().state === "waiting" ? theme().palette.gold : theme().palette.working)}>{status()}</Text>}
      {!status() && panel().empty && panel().link === "live" && <>
        <Text style={text(13)}>{panel().toolbar ? `${panel().toolbar!.providerLabel} chat` : "Chat"}</Text>
        {panel().cwd && <Text style={mono(11, theme().palette.muted)}>{panel().cwd}</Text>}
        <Text style={text(12, theme().palette.muted)}>Type a message below to begin.</Text>
      </>}
    </View>;
  }

  // A boolean, so a panel delivery never rebuilds the transcript and composer.
  const deleted = computed(() => panel().link === "deleted");
  return <View testID={`chat:${props.chatId}`} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.bg }}>
    {deleted() ? <View testID="chat-deleted" style={{ flex: 1, padding: 24, gap: 8 }}>
      <Text style={text(14)}>This chat was deleted</Text>
      <Text style={text(12, theme().palette.muted)}>Its history is gone from the chat host.</Text>
      {props.onClose && <View role="button" accessibilityLabel="Close" onPress={props.onClose}><Text style={text(12, theme().palette.cyan)}>Close</Text></View>}
    </View> : <>
      <List testID="chat-transcript" ref={(node) => { list = node; }} style={{ flex: 1, minHeight: 0 }}
        each={rows()} keyExtractor={(row) => row.key} estimatedItemSize={space(28, theme().scale)}
        maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 48 }}
        onScrollDragStart={() => { following = false; }}
        onViewableItemsChange={(info) => {
          const range = viewport(info.viewableItems.map((token) => token.index));
          if (range) handle.setViewport(range[0], range[1]);
          report();
        }}
        footer={() => <Footer />}>
        {(row) => <Row row={row} />}
      </List>
      <Requests panel={panel} handle={handle} theme={theme} notice={notice} />
      {notices().length > 0 && <View testID="chat-notices" style={{ marginHorizontal: space(14, theme().scale), gap: 2 }}>
        {notices().map((message, index) => <View style={{ flexDirection: "row", gap: 8, paddingVertical: 4 }}>
          <Text testID="chat-notice" style={{ ...text(11, theme().palette.gold), flex: 1 }}>{message}</Text>
          <View role="button" accessibilityLabel="Dismiss notice" onPress={() => notices.set(notices().filter((_, at) => at !== index))}>
            <Text style={text(11, theme().palette.gold)}>×</Text>
          </View>
        </View>)}
      </View>}
      {panel().notices.map((banner) => <View testID={`chat-banner:${banner.id}`} style={{ marginHorizontal: space(14, theme().scale), paddingVertical: 4 }}>
        <Text style={text(11, banner.level === "error" ? ERROR : banner.level === "warning" ? theme().palette.gold : theme().palette.muted)}>{banner.text}</Text>
      </View>)}
      {panel().state === "stopped" && <Text style={{ ...text(11, theme().palette.muted), marginHorizontal: space(14, theme().scale) }}>Stopped. The chat will resume when you send a message.</Text>}
      {panel().state === "failed" && <Text style={{ ...text(11, theme().palette.gold), marginHorizontal: space(14, theme().scale) }}>{`Failed: ${panel().stateMessage ?? ""}`}</Text>}
      <Composer panel={panel} handle={handle} theme={theme} notice={notice} visible={() => props.visible}
        onSent={() => { following = true; list?.scrollToEnd(); }} onEditor={(editor) => editors.push(editor)} />
    </>}
  </View>;
}

interface PartProps {
  panel: () => ChatPanel;
  handle: ChatHandle;
  theme: () => InterfaceTheme;
  notice: (message: string) => void;
}

/** Every panel delivery is a new object; equal content is no change. */
const sameContent = <T,>(previous: T, next: T) => previous === next || JSON.stringify(previous) === JSON.stringify(next);

/** Approvals and questions stay visible and actionable above the composer.
 * Each is selected by content and mounted per request, in a View of its own:
 * expressions returned in a fragment re-run with the parent's other ones. A
 * panel delivery that leaves a request alone keeps its expanded, picked and
 * typed state. */
function Requests(props: PartProps) {
  // What a mounted request shows. It keeps the last request through the
  // delivery that ends it: its own expressions may read it before the gate,
  // keyed by request id, takes it away.
  const approval = computed<ChatApproval | undefined>((last) => props.panel().approvals[0] ?? last, { equals: sameContent });
  const question = computed<ChatQuestion | undefined>((last) => props.panel().questions[0] ?? last, { equals: sameContent });
  const more = computed(() => props.panel().approvals.length - 1);
  const approvalId = computed(() => props.panel().approvals[0]?.requestId);
  const questionId = computed(() => props.panel().questions[0]?.requestId);
  return <>
    <View>{approvalId() !== undefined && <ApprovalBar {...props} approval={() => approval()!} more={more} />}</View>
    <View>{questionId() !== undefined && <QuestionPanel {...props} question={() => question()!} />}</View>
  </>;
}

/** Requests answered but not yet resolved by the host; repeated clicks do nothing. */
const answered = new Set<string>();

function ApprovalBar(props: PartProps & { approval: () => ChatApproval; more: () => number }) {
  const theme = props.theme;
  const expanded = signal(false);
  const busy = signal(false);
  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const detail = () => detailLines(props.approval().detail, expanded());
  const decide = (decision: string) => {
    const id = props.approval().requestId;
    if (answered.has(id)) return;
    answered.add(id);
    busy.set(true);
    props.handle.approve(id, decision).catch((error) => {
      answered.delete(id);
      props.notice(`Could not reach the chat: ${error}`);
    }).finally(() => busy.set(false));
  };
  return <View testID="chat-approval" accessibilityLabel={props.approval().heading} style={{ marginHorizontal: space(14, theme().scale), marginBottom: 4, padding: 8, gap: 4, borderRadius: 6, borderWidth: 1, borderColor: theme().palette.gold, backgroundColor: theme().palette.panel }}>
    <View style={{ flexDirection: "row", gap: 8 }}>
      <Text style={text(11, theme().palette.gold)}>{`✋ ${props.approval().heading}`}</Text>
      {props.more() > 0 && <Text style={text(11, theme().palette.muted)}>{`${props.more()} more waiting`}</Text>}
    </View>
    <Text numberOfLines={2} style={{ fontFamily: props.approval().kind === "command" ? theme().monoFamily : theme().family, fontSize: 12 * theme().scale, color: theme().palette.text }}>{props.approval().title}</Text>
    {detail().lines.length > 0 && <Text style={text(11, theme().palette.muted)}>{detail().lines.join("\n")}</Text>}
    {(detail().hidden > 0 || expanded()) && <View role="button" accessibilityLabel={expanded() ? "show less" : `show ${detail().hidden} more lines`} onPress={() => expanded.set(!expanded())}>
      <Text style={text(11, theme().palette.cyan)}>{expanded() ? "show less" : `show ${detail().hidden} more lines`}</Text>
    </View>}
    <View style={{ flexDirection: "row", gap: 8, flexWrap: "wrap" }}>
      {props.approval().choices.map((choice) => <View role="button" testID={`chat-decision:${choice.decision}`} accessibilityLabel={choice.label}
        accessibilityState={{ disabled: busy() }} onPress={() => decide(choice.decision)}
        style={{ paddingHorizontal: 10, paddingVertical: 4, borderRadius: 4, opacity: busy() ? 0.5 : 1,
          backgroundColor: choice.decision === "accept" || choice.decision === "accept_for_session" ? theme().palette.panelActive : "transparent",
          borderWidth: 1, borderColor: choice.decision === "decline" || choice.decision === "cancel" ? ERROR : theme().palette.cyan }}>
        <Text style={text(12, choice.decision === "decline" || choice.decision === "cancel" ? ERROR : theme().palette.cyan)}>{choice.key ? `${choice.label}  ${choice.key}` : choice.label}</Text>
      </View>)}
    </View>
    <Text style={text(10, theme().palette.muted)}>With the message box empty: ⏎ allows, ⎋ denies. ⇧⏎ and ⌥⏎ insert a new line.</Text>
  </View>;
}

function QuestionPanel(props: PartProps & { question: () => ChatQuestion }) {
  const theme = props.theme;
  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const picked = signal<string[][]>([]);
  const typed = signal<string[]>([]);
  const sent = signal(false);
  let request = "";
  effect(() => {
    // A new request starts empty; typed answers survive refreshes of the same one.
    if (props.question().requestId !== request) {
      request = props.question().requestId;
      picked.set(props.question().prompts.map(() => []));
      typed.set(props.question().prompts.map(() => ""));
      sent.set(false);
    }
  });
  const toggle = (prompt: number, label: string, multi: boolean) => {
    const next = picked().map((labels) => [...labels]);
    const current = next[prompt] ?? [];
    next[prompt] = current.includes(label) ? current.filter((value) => value !== label) : multi ? [...current, label] : [label];
    picked.set(next);
  };
  const submit = () => {
    if (sent()) return;
    const answers = answersOf(picked(), typed());
    if (!answers) { props.notice("Answer every question first."); return; }
    sent.set(true);
    void props.handle.answer({ requestId: props.question().requestId, answers }).then((receipt) => {
      if (receipt.status !== "submitted") {
        sent.set(false);
        props.notice(`Answer submission retained: ${receipt.error ?? receipt.status}`);
      }
    });
  };
  return <View testID="chat-question" style={{ marginHorizontal: space(14, theme().scale), marginBottom: 4, padding: 8, gap: 6, borderRadius: 6, borderWidth: 1, borderColor: theme().palette.cyan, backgroundColor: theme().palette.panel }}>
    {props.question().prompts.map((prompt, index) => <View style={{ gap: 4 }}>
      {prompt.header && <Text style={text(10, theme().palette.muted)}>{prompt.header}</Text>}
      <Text style={text(12)}>{prompt.question}</Text>
      <View style={{ flexDirection: "row", gap: 6, flexWrap: "wrap" }}>
        {prompt.options.map((option) => <View role={prompt.multiSelect ? "checkbox" : "radio"} testID={`chat-option:${index}:${option.label}`}
          accessibilityLabel={option.label} accessibilityState={{ checked: (picked()[index] ?? []).includes(option.label) }}
          onPress={() => toggle(index, option.label, prompt.multiSelect)}
          style={{ paddingHorizontal: 8, paddingVertical: 3, borderRadius: 4, borderWidth: 1, borderColor: (picked()[index] ?? []).includes(option.label) ? theme().palette.cyan : theme().palette.divider }}>
          <Text style={text(11)}>{`${(picked()[index] ?? []).includes(option.label) ? "✓ " : ""}${option.description ? `${option.label}: ${option.description}` : option.label}`}</Text>
        </View>)}
      </View>
      <Input testID={`chat-answer:${index}`} placeholder="or type an answer" readonly={sent()} value={typed()[index] ?? ""}
        onChange={(value) => { const next = [...typed()]; next[index] = value; typed.set(next); }} onSubmit={submit}
        style={{ ...text(11), paddingHorizontal: 6, paddingVertical: 3, borderWidth: 1, borderRadius: 3, borderColor: theme().palette.divider }} />
    </View>)}
    <View role="button" testID="chat-send-answers" accessibilityLabel={sent() ? "Sent" : "Send answers"} onPress={submit}>
      <Text style={text(12, theme().palette.cyan)}>{sent() ? "Sent" : "Send answers  ⏎"}</Text>
    </View>
  </View>;
}

/** The message box, its send/stop slot and the model, effort, Fast and mode
 * choices. Drafts are saved on every change and restored into an empty box. */
function Composer(props: PartProps & { visible: () => boolean; onSent: () => void; onEditor: (editor: number) => void }) {
  const theme = props.theme;
  // One draft editor identity per mounted composer, unique across windows: the
  // native draft owners are process-wide (CHATC-035).
  const editor = Chat.newEditor();
  props.onEditor(editor);
  let generation = 0;
  const draft = signal("");
  const pending = signal(false);
  const composing = signal(false);
  const focused = signal(false);
  const saved = signal<ChatDraft["submissions"]>([]);
  let lastSend = 0;
  let input: YNode | undefined;
  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const toolbar = () => props.panel().toolbar;
  const empty = () => draft().trim() === "";
  const accepting = () => props.panel().link !== "deleted" && props.panel().link !== "connecting";
  const approval = () => props.panel().approvals[0];
  const guarded = () => Date.now() - lastSend < APPROVAL_GUARD_MS;

  // Restore the saved draft only into an empty composer (CHATC-052).
  void props.handle.restoreDraft().then((restored) => {
    if (!restored) return;
    if (draft() === "" && restored.text) { draft.set(restored.text); generation += 1; }
    saved.set(restored.submissions.filter((submission) => submission.status === "refused" || submission.status === "uncertain"));
    for (const warning of restored.warnings) props.notice(warning);
  }).catch(() => {});

  function edit(value: string) {
    draft.set(value);
    generation += 1;
    props.handle.saveDraft(value, editor, generation);
  }

  function answer(decision: Decision | null) {
    const request = approval();
    if (!request || !decision || answered.has(request.requestId)) return;
    answered.add(request.requestId);
    void props.handle.approve(request.requestId, decision).catch((error) => {
      answered.delete(request.requestId);
      props.notice(`Could not reach the chat: ${error}`);
    });
  }

  function submit() {
    if (composing()) return;
    if (empty()) {
      // ⏎ in an empty box answers a waiting approval with Allow (CHATT-207).
      const request = approval();
      if (request && !guarded()) answer(decisionForKey("enter", request.choices.map((choice) => choice.decision)));
      return;
    }
    if (pending() || !accepting()) return;
    const sending = draft();
    const sentGeneration = generation;
    pending.set(true);
    lastSend = Date.now();
    props.onSent();
    void props.handle.submit({ text: sending, editor, generation: sentGeneration }).then((receipt) => {
      if (receipt.status === "submitted") {
        // A newer edit (another generation) is kept (CHATC-035).
        if (generation === sentGeneration) { draft.set(""); generation += 1; }
        else props.handle.saveDraft(draft(), editor, generation);
        return;
      }
      props.handle.saveDraft(draft(), editor, generation);
      props.notice(receipt.status === "refused"
        ? `Submission refused: ${receipt.error ?? ""}. Your draft is retained.`
        : `Submission outcome uncertain: ${receipt.error ?? ""}. Inspect the transcript before explicitly resending. Your draft is retained.`);
      if (receipt.key) saved.set([...saved(), { key: receipt.key, text: sending, status: receipt.status, error: receipt.error }]);
    }).finally(() => pending.set(false));
  }

  function interrupt() {
    void props.handle.interrupt().catch((error) => props.notice(`Could not reach the chat: ${error}`));
  }

  // ⎋ in the focused, empty box denies a waiting approval (CHATT-207).
  effect(() => {
    const request = approval();
    if (!props.visible() || !focused() || !empty() || !request || composing()) return;
    return keymap.bind("escape", () => {
      if (!guarded()) answer(decisionForKey("escape", request.choices.map((choice) => choice.decision)));
    }, { when: "always" });
  });

  const configure = (request: Parameters<ChatHandle["configure"]>[0]) =>
    void props.handle.configure(request).catch((error) => props.notice(`Could not reach the chat: ${error}`)).finally(() => input?.focus());

  function modelMenu() {
    const bar = toolbar();
    if (!bar) return;
    const items: MenuItem[] = bar.models.length
      ? bar.models.map((model) => ({ label: model.label, checked: model.current, onSelect: () => {
        // An effort the new model does not take moves to its default (CHATC-164).
        configure({ model: model.id });
      } }))
      : bar.modelSuggestions.map((name) => ({ label: name, checked: bar.model === name, onSelect: () => configure({ model: name }) }));
    items.push({ type: "separator" });
    for (const provider of bar.providers) {
      items.push({ label: provider.label, disabled: props.panel().running, onSelect: () => {
        void props.handle.switchProvider(provider.id, null).catch((error) => props.notice(String(error)));
      } });
    }
    void Menu.popup(items);
  }

  function effortMenu() {
    const bar = toolbar();
    if (!bar) return;
    void Menu.popup(bar.efforts.map((effort) => ({ label: effort.label, checked: effort.current, onSelect: () => configure({ effort: effort.id }) })));
  }

  function modeMenu() {
    const bar = toolbar();
    if (!bar) return;
    void Menu.popup(bar.modes.map((mode) => ({ label: mode.label, checked: mode.current, onSelect: () => configure({ approvalMode: mode.id as "supervised" }) })));
  }

  const pill = (testID: string, label: string, onPress: () => void, tint = theme().palette.text) =>
    <View role="button" testID={testID} accessibilityLabel={label} onPress={onPress}
      style={{ height: 26, paddingHorizontal: 8, justifyContent: "center", borderRadius: 13 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text numberOfLines={1} style={text(11, tint)}>{label}</Text>
    </View>;

  const stopping = () => props.panel().running && (empty() || pending());
  return <View testID="chat-composer" style={{ margin: space(14, theme().scale), marginTop: 4, padding: 8, gap: 4, borderRadius: 10, borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.panel }}>
    {saved().map((submission) => <View testID={`chat-saved:${submission.key}`} style={{ gap: 2, paddingBottom: 4 }}>
      <Text style={text(11, theme().palette.gold)}>{submission.status === "refused" ? `Submission refused${submission.error ? `: ${submission.error}` : ""}` : `Submission outcome uncertain${submission.error ? `: ${submission.error}` : ""}`}</Text>
      <View style={{ flexDirection: "row", gap: 12 }}>
        <View role="button" accessibilityLabel="Replace current draft with saved" onPress={() => { if (!pending()) edit(submission.text); }}><Text style={text(11, theme().palette.cyan)}>Replace current draft with saved</Text></View>
        <View role="button" accessibilityLabel="Dismiss saved submission" onPress={() => { props.handle.dismissSubmission(submission.key); saved.set(saved().filter((other) => other.key !== submission.key)); }}><Text style={text(11, theme().palette.cyan)}>Dismiss saved submission</Text></View>
      </View>
    </View>)}
    <Input ref={(node) => { input = node; }} testID="chat-input" accessibilityLabel="Chat message" placeholder="Message"
      multiline={true} submitBehavior="submit" readonly={!accepting()} value={draft()}
      onChange={edit} onSubmit={submit} onFocus={() => focused.set(true)} onBlur={() => focused.set(false)}
      onCompositionChange={(event) => composing.set(event.composing)}
      style={{ ...text(12), minHeight: 19, maxHeight: 8 * 19 * theme().scale, paddingHorizontal: 4 }} />
    <View style={{ flexDirection: "row", alignItems: "center", gap: 4 }}>
      {toolbar() && pill("chat-model", toolbar()!.modelLabel, modelMenu)}
      {toolbar()?.effortAvailable && pill("chat-effort", toolbar()!.effortLabel, effortMenu)}
      {toolbar()?.fastAvailable && pill("chat-fast", toolbar()!.fast ? "⚡ Fast" : "Fast", () => configure({ fast: !toolbar()!.fast }), toolbar()!.fast ? theme().palette.cyan : theme().palette.muted)}
      {toolbar() && pill("chat-mode", toolbar()!.modeLabel, modeMenu, theme().palette.muted)}
      <View style={{ flex: 1 }} />
      {stopping()
        ? <View role="button" testID="chat-interrupt" accessibilityLabel={STOP_TIP} onPress={interrupt}
          style={{ width: 26, height: 26, borderRadius: 13, alignItems: "center", justifyContent: "center", backgroundColor: theme().palette.gold }}>
          <Text style={text(11, theme().palette.bg)}>■</Text>
        </View>
        : <View role="button" testID="chat-send" accessibilityLabel={props.panel().running ? STEER_TIP : SEND_TIP}
          accessibilityState={{ disabled: empty() || pending() || !accepting() }} onPress={submit}
          style={{ width: 26, height: 26, borderRadius: 13, alignItems: "center", justifyContent: "center",
            backgroundColor: empty() || pending() ? theme().palette.divider : theme().palette.cyan }}>
          <Text style={text(12, theme().palette.bg)}>↑</Text>
        </View>}
    </View>
  </View>;
}
