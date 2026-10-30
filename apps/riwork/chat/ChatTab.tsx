import { Clipboard, Image, List, Scroll, Text, View, cleanup, computed, effect, host, keymap, openUrl, signal, type YNode } from "nsh";
import { Input, TextView } from "nsh/base";
import { Menu, type MenuItem } from "nsh/desktop";
import {
  Chat, Settings, type ChatAttachment, type ChatApproval, type ChatBody, type ChatDraft, type ChatHandle, type ChatPanel,
  type ChatQuestion, type ChatRow, type ChatSearchHit, type ChatImagePreview,
} from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { Icon, type IconName } from "../Icon";
import { CHAT_COLUMN_WIDTH, chatColumn, chatGutter, chatHairline } from "./design";
import {
  APPROVAL_GUARD_MS, SEND_TIP, STEER_TIP, STOP_TIP, answersOf, applyRows, bodyChanges,
  decisionForKey, detailLines, emptyPanel, viewport, type Decision,
} from "./model";

import { createBillingControl, BillingControls, BillingNoticeAction, type BillingControl } from "./BillingControls";
import { DictationControl, DictationStatus, type DictationController } from "./Dictation";
import { FileReview } from "./FileReview";
import { ChatImage } from "./ChatImage";
import { WorkerShelf } from "./WorkerShelf";
import { MemoPreview, appendMemo, createMemoPreview, memoCommandRest } from "./MemoPreview";
import { CodeTerminalDialog } from "./CodeTerminalDialog";
import { CodeReader, MAX_SNIPPET, saveSnippet, type CodeSnippet } from "./CodeReader";
import { Suggestions, createSuggestions } from "./Suggestions";
import { utf8Bytes } from "../files_model";

const ERROR = "#ef6b73";

export interface NativeChatUsage {
  provider: string;
  usageLabel?: string;
  usageDetail?: string;
}

export interface ChatTabProps {
  chatId: string;
  visible: boolean;
  active?: boolean;
  theme?: () => InterfaceTheme;
  onTitle?: (title: string) => void;
  onUsage?: (usage: NativeChatUsage) => void;
  onClose?: () => void;
  onLink?: (target: string, line?: number) => void;
  onOpenWorker?: (id: string, kind: "shell" | "chat") => void;
  onApiKeySettings?: () => void;
  onEditMemo?: (projectId: string) => void;
  terminalTargets?: () => { id: string; label: string }[];
  onCopyToTerminal?: (id: string, text: string) => Promise<void>;
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
  /** Body views and image cards ever made: streaming updates them in place. */
  bodyViews: number;
  imageCards: number;
  /** The composer's laid-out heights: its box, message box and controls. */
  composer: { box: number; input: number; controls: number };
}

/** A chat tab: the transcript List over the bridge's rows, the requests that
 * wait for the user, and the composer. Rows are headers; a row's body comes by
 * viewport and updates on its own signal, so streaming re-renders one cell. */
export function ChatTab(props: ChatTabProps) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const active = () => props.visible && props.active !== false;
  const handle: ChatHandle = Chat.open(props.chatId);
  cleanup(() => handle.close());
  const rows = signal<ChatRow[]>([]);
  const paneWidth = signal(0);
  const reviewing = signal<{ scope?: string; key?: string } | null>(null);
  const imagePreview = signal<ChatImagePreview | null>(null);
  const codeSnippet = signal<string | null>(null);
  const codeReader = signal<{ snippet: CodeSnippet; directory?: string } | null>(null);
  const exporting = signal(false);
  let codeReturnFocus: YNode | undefined;
  const interactive = () => active() && !reviewing() && !imagePreview() && codeSnippet() === null && codeReader() === null && !exporting();
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
  let bodyViews = 0;
  let imageCards = 0;
  const composer = { box: 0, input: 0, controls: 0 };
  const editors: number[] = [];
  let list: YNode | undefined;
  const following = signal(true);
  const firstVisible = signal(0);
  const selectedRow = signal<string | null>(null);
  const finding = signal(false);
  const query = signal("");
  const hits = signal<ChatSearchHit[]>([]);
  const hitIndex = signal(-1);
  const searching = signal(false);
  const showUsage = signal(false);
  let searchInput: YNode | undefined;
  let pendingReveal: string | null = null;
  let searchGeneration = 0;
  let alive = true;
  cleanup(() => { alive = false; searchGeneration++; });
  const report = () => props.onDiagnostics?.({
    rows: rows().length, cells, constructed, bodies: [...bodies.values()].filter((body) => body() !== undefined).length,
    groups: rows().filter((row) => row.kind === "group" && !row.open).map((row) => row.id), panel: panel(),
    editors: [...editors], bodyViews, imageCards, composer: { ...composer },
  });
  const subscription = handle.subscribe((delivery) => {
    if (delivery.kind === "reset") for (const body of bodies.values()) body.set(undefined);
    for (const [key, body] of bodyChanges((key) => bodies.get(key)?.(), delivery)) bodySignal(key).set(body);
    const next = applyRows(rows(), delivery);
    if (next !== rows()) rows.set(next);
    if (delivery.panel) {
      panel.set(delivery.panel);
      props.onTitle?.(delivery.panel.title);
      props.onUsage?.({ provider: delivery.panel.provider ?? "unknown", usageLabel: delivery.panel.usageLabel, usageDetail: delivery.panel.usageDetail });
      if (delivery.panel.failures.length) notices.set([...notices(), ...delivery.panel.failures].slice(-3));
    }
    if (pendingReveal) {
      const at = next.findIndex((row) => row.id === pendingReveal);
      if (at >= 0) { list?.scrollToIndex({ index: at, viewPosition: 0 }); pendingReveal = null; }
    } else if (delivery.kind === "reset" && following()) list?.scrollToEnd();
    report();
  });
  cleanup(() => subscription.close());

  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const mono = (base = 11, color = theme().palette.text) => ({ fontFamily: theme().monoFamily, fontSize: base * theme().scale, color });
  const notice = (message: string) => notices.set([...notices(), message].slice(-3));
  const billing = createBillingControl({ panel, handle, visible: () => props.visible, onError: notice, onNotice: notice, onOpenSettings: () => props.onApiKeySettings?.() });

  function jumpTo(index: number) {
    if (index < 0 || index >= rows().length) return;
    following.set(false);
    firstVisible.set(index);
    selectedRow.set(rows()[index].id);
    handle.setViewport(index, Math.min(rows().length - 1, index + 12));
    list?.scrollToIndex({ index, viewPosition: 0 });
  }

  function latest() {
    following.set(true); selectedRow.set(null);
    list?.scrollToEnd();
  }

  // The user rows' indices: equal while a turn streams, so the message bar
  // and its controls stay as they are.
  const prompts = computed(() => rows().flatMap((row, index) => row.kind === "user" ? [index] : []),
    { equals: (previous, next) => previous.length === next.length && previous.every((index, at) => index === next[at]) });
  const currentPrompt = computed(() => {
    const entries = prompts();
    let current = 0;
    for (let at = 0; at < entries.length; at++) { if (entries[at] <= firstVisible()) current = at; else break; }
    return current;
  });
  // What the header, footer and banners show of a panel. Every delivery is a
  // new panel; expressions that read these only run again on a real change.
  const mode = computed(() => panel().mode);
  const usageLabel = computed(() => panel().usageLabel);
  const hasUsageDetail = computed(() => !!panel().usageDetail);
  const hasFileChanges = computed(() => !!panel().fileChanges);

  async function copy(id?: string) {
    try { await Clipboard.writeText(await handle.readText(id)); }
    catch (error) { notice(`Could not copy: ${error}`); }
  }

  async function openLink(target: string) {
    try {
      if (/^(https?:|mailto:)/i.test(target)) {
        if (!await openUrl(target)) notice("No application could open this link.");
      } else if (!target.startsWith("#")) {
        const link = await handle.resolveLink(target);
        if (props.onLink) props.onLink(link.path, link.line);
        else notice(`File: ${link.path}${link.line ? `:${link.line}` : ""}`);
      }
    } catch (error) { notice(`Could not open link: ${error}`); }
  }

  function revealHit(index: number) {
    const found = hits();
    if (!found.length) return;
    const at = (index + found.length) % found.length;
    hitIndex.set(at);
    const id = found[at].id;
    following.set(false); selectedRow.set(id);
    const visible = rows().findIndex((row) => row.id === id && (!row.disclosure || row.open));
    if (visible >= 0) { jumpTo(visible); return; }
    pendingReveal = id;
    void handle.revealItem(id).then((row) => {
      if (!alive || selectedRow() !== id) return;
      if (row === null || row === undefined) { pendingReveal = null; notice("This result is no longer in the transcript."); }
      else {
        const at = rows().findIndex((item) => item.id === id && (!item.disclosure || item.open));
        if (at >= 0) { pendingReveal = null; jumpTo(at); }
      }
    }).catch((error) => { pendingReveal = null; notice(String(error)); });
  }

  effect(() => {
    const text = query().trim();
    const token = ++searchGeneration;
    hitIndex.set(-1); hits.set([]);
    if (!finding() || !text) { searching.set(false); return; }
    searching.set(true);
    const timer = setTimeout(() => {
      void handle.search(text).then((results) => {
        if (!alive || token !== searchGeneration) return;
        hits.set(results); searching.set(false);
        if (results.length) revealHit(0);
      }).catch((error) => { if (alive && token === searchGeneration) { searching.set(false); notice(String(error)); } });
    }, 150);
    return () => clearTimeout(timer);
  });

  function closeImage() { imagePreview.set(null); }
  function closeCodeSnippet() {
    codeSnippet.set(null);
    if (active()) codeReturnFocus?.focus();
    codeReturnFocus = undefined;
  }
  effect(() => {
    if (!active() || !imagePreview()) return;
    return keymap.bind("escape", closeImage, { when: "always" });
  });
  function findInChat() { finding.set(true); searchInput?.focus(); }
  function closeFind() { finding.set(false); selectedRow.set(null); }
  effect(() => {
    if (!interactive()) return;
    return keymap.bind({
      "mod+f": findInChat,
      "mod+g": () => { if (finding()) revealHit(hitIndex() + 1); },
      "mod+shift+g": () => { if (finding()) revealHit(hitIndex() - 1); },
    });
  });

  cleanup(host.on("command", (data) => {
    if (!interactive()) return;
    const command = data as { id?: string; args?: { query?: string } };
    if (command.id === "find" || command.id === "search" || command.id === "findInChat") {
      if (command.args?.query !== undefined) query.set(command.args.query);
      findInChat();
    } else if (command.id === "findNext") revealHit(hitIndex() + 1);
    else if (command.id === "findPrevious") revealHit(hitIndex() - 1);
    else if (command.id === "findClose" || (command.id === "chatEscape" && finding())) closeFind();
    else if (command.id === "toggleTranscriptMode") handle.setMode(panel().mode === "normal" ? "verbose" : "normal");
    else if (command.id === "expandTranscript") handle.expandAll(true);
    else if (command.id === "collapseTranscript") handle.expandAll(false);
    else if (command.id === "interruptChat" && panel().running) void handle.interrupt().catch((error) => notice(String(error)));
  }));

  // A label that changes (an accessor) updates the control in place.
  const control = (label: string | (() => string), action: () => void, active = false, disabled = false) =>
    <View role="button" accessibilityLabel={typeof label === "function" ? label() : label} accessibilityState={{ disabled, selected: active }}
      onPress={() => { if (!disabled) action(); }}
      style={{ paddingHorizontal: 8, height: 27, alignItems: "center", justifyContent: "center", borderRadius: 5,
        backgroundColor: active ? theme().palette.panelActive : "transparent", opacity: disabled ? 0.35 : 1 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text numberOfLines={1} style={text(11, active ? theme().palette.text : theme().palette.muted)}>{typeof label === "function" ? label() : label}</Text>
    </View>;

  const iconControl = (name: IconName, label: string, action: () => void, active = false, disabled = false) =>
    <View role="button" focusable={!disabled} accessibilityLabel={label} tooltip={label} accessibilityState={{ disabled, selected: active }}
      onPress={() => { if (!disabled) action(); }}
      style={{ width: 28, height: 28, flexShrink: 0, alignItems: "center", justifyContent: "center", borderRadius: 5,
        backgroundColor: active ? theme().palette.panelActive : "transparent", opacity: disabled ? 0.35 : 1 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Icon name={name} size={16} color={active ? theme().palette.text : theme().palette.muted} />
    </View>;

  function moreMenu() {
    const items: MenuItem[] = [
      { label: "Find in conversation…", onSelect: findInChat },
      ...(panel().fileChanges ? [{ label: "Review changed files…", onSelect: () => reviewing.set({}) }] : []),
      ...(panel().usageLabel ? [{ label: `Context usage · ${panel().usageLabel}`, checked: showUsage(), onSelect: () => showUsage.set(!showUsage()) }] : []),
      { label: "Copy conversation as Markdown", onSelect: () => void copy() },
      { label: "Copy chat ID", onSelect: () => void Clipboard.writeText(props.chatId) },
    ];
    if (panel().sessionId) items.push({ label: "Copy provider session ID", onSelect: () => void Clipboard.writeText(panel().sessionId!) });
    items.push({ type: "separator" },
      { label: "Expand all activity", onSelect: () => handle.expandAll(true) },
      { label: "Collapse all activity", onSelect: () => handle.expandAll(false) },
      { label: "Jump to latest", onSelect: latest },
      { type: "separator" },
      { label: "Stop agent process", disabled: panel().state === "stopped", onSelect: () => { void handle.stop().catch((error) => notice(String(error))); } });
    void Menu.popup(items);
  }

  function ChatHeader() {
    const compact = computed(() => paneWidth() > 0 && paneWidth() < 560 * theme().scale);
    const hasUsage = computed(() => !!usageLabel() && !compact());
    const several = computed(() => prompts().length > 1);
    return <View style={{ borderBottomWidth: 1, borderColor: chatHairline(theme()), paddingHorizontal: 12, paddingVertical: 7, gap: 5 }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 6, minWidth: 0, flexShrink: 1 }}>
          <View accessible={false} style={{ width: 5, height: 5, borderRadius: 3, flexShrink: 0,
            backgroundColor: panel().state === "waiting" ? theme().palette.gold : panel().running ? theme().palette.working : theme().palette.muted }} />
          <Text numberOfLines={1} style={{ ...text(12), fontWeight: "600", flexShrink: 1 }}>{panel().toolbar?.providerLabel ?? "Chat"}</Text>
        </View>
        <View style={{ width: 1, height: 14, marginHorizontal: 4, backgroundColor: chatHairline(theme()) }} />
        {compact() ? control(mode() === "normal" ? "Normal ▾" : "Verbose ▾", () => {
          void Menu.popup([
            { label: "Normal", checked: mode() === "normal", onSelect: () => handle.setMode("normal") },
            { label: "Verbose", checked: mode() === "verbose", onSelect: () => handle.setMode("verbose") },
          ]);
        }) : <View style={{ flexDirection: "row", gap: 2, padding: 2, borderRadius: 7, backgroundColor: theme().palette.panel }}>
          {control("Normal", () => handle.setMode("normal"), mode() === "normal")}
          {control("Verbose", () => handle.setMode("verbose"), mode() === "verbose")}
        </View>}
        <View style={{ flex: 1, minWidth: 0 }} />
        {hasUsage() && control(() => usageLabel() ?? "", () => showUsage.set(!showUsage()), showUsage())}
        {hasFileChanges() && !compact() && iconControl("file", "Review changed files", () => reviewing.set({}))}
        {iconControl("search", "Find in conversation · ⌘F", findInChat, finding())}
        {iconControl("more", "Conversation options", moreMenu)}
      </View>
      {showUsage() && hasUsageDetail() && <Text selectable={true} style={mono(10, theme().palette.muted)}>{panel().usageDetail ?? ""}</Text>}
      {several() && <View style={{ flexDirection: "row", alignItems: "center", gap: 2 }}>
        {iconControl("up", "Previous message", () => jumpTo(prompts()[currentPrompt() - 1] ?? -1), false, currentPrompt() === 0)}
        {control(`Message ${currentPrompt() + 1} of ${prompts().length}`, () => {
          void handle.navigation().then((messages) => Menu.popup(messages.map((message, at) => ({
            label: `${message.label} · ${message.preview}`, checked: at === currentPrompt(),
            onSelect: () => jumpTo(rows().findIndex((row) => row.id === message.id)),
          })))).catch((error) => notice(String(error)));
        })}
        {iconControl("down", "Next message", () => jumpTo(prompts()[currentPrompt() + 1] ?? -1), false, currentPrompt() >= prompts().length - 1)}
        <View style={{ flex: 1 }} />
        {!following() && control("Latest ↓", latest)}
      </View>}
      {finding() && <View style={{ gap: 4 }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 4 }}>
          <Input ref={(node) => { searchInput = node; node.focus(); }} value={query()} onChange={(value) => query.set(value)} onSubmit={() => revealHit(hitIndex() + 1)}
            placeholder="Find in conversation" accessibilityLabel="Find in conversation" style={{ ...text(12), flex: 1, minWidth: 50, height: 28, paddingHorizontal: 7, borderWidth: 1, borderRadius: 4, borderColor: theme().palette.divider }} />
          <Text style={text(10, theme().palette.muted)}>{searching() ? "Searching…" : query().trim() ? hits().length ? `${hitIndex() + 1} / ${hits().length}${hits().length === 200 ? "+" : ""}` : "No matches" : ""}</Text>
          {iconControl("up", "Previous match", () => revealHit(hitIndex() - 1), false, !hits().length)}
          {iconControl("down", "Next match", () => revealHit(hitIndex() + 1), false, !hits().length)}
          {iconControl("close", "Close search", closeFind)}
        </View>
        {hits()[hitIndex()] && <Text numberOfLines={2} style={text(11, theme().palette.muted)}>{`${hits()[hitIndex()].label} · ${hits()[hitIndex()].preview}`}</Text>}
      </View>}
    </View>;
  }

  // ⌘. interrupts from anywhere in a visible chat, only while it runs (CHATT-020).
  effect(() => {
    if (!interactive() || !panel().running) return;
    return keymap.bind("mod+.", () => { void handle.interrupt().catch((error) => notice(`Could not reach the chat: ${error}`)); }, { when: "always" });
  });
  // ⌘⇧V switches Normal/Verbose; ⌘⌥→/← expand or collapse (CHATT-031).
  effect(() => {
    if (!interactive()) return;
    return keymap.bind({
      "mod+shift+v": () => { following.set(true); handle.setMode(panel().mode === "normal" ? "verbose" : "normal"); },
      "mod+alt+right": () => handle.expandAll(true),
      "mod+alt+left": () => handle.expandAll(false),
    });
  });

  // A cell keeps its nodes while its row streams. The runtime runs a JSX
  // expression again whenever anything it reads changes and makes what it
  // returns again, so a branch reading `row()` would be remade on every clock
  // tick, and a body on every delta. Each branch reads a memo that changes
  // only with the branch (a kind, a flag); changing text updates the nodes in
  // place. Leaf and Body return one function: the functions of a returned
  // fragment are no memos of their own and run again with the parent's other
  // expressions.
  function Row(rowProps: { row: () => ChatRow }) {
    cells += 1;
    constructed += 1;
    cleanup(() => { cells -= 1; });
    const row = rowProps.row;
    const indent = () => chatGutter(paneWidth(), theme().scale) + Math.min(row().depth, paneWidth() < 520 ? 2 : 4) * space(12, theme().scale);
    const disclosure = computed(() => row().disclosure);
    const showBody = computed(() => row().disclosure && row().open && row().hasBody);
    const artifacts = computed<NonNullable<ChatRow["artifacts"]>>(() => row().kind === "edit" ? [] : row().artifacts ?? [], { equals: sameContent });
    const hasArtifacts = computed(() => artifacts().length > 0);
    // This cell's image previews, read once: a running activity row gains
    // screenshots, which makes its strip again.
    const previews = new Map<string, ChatImagePreview>();
    return <View testID={`chat-row:${row().key}`} style={{ ...chatColumn(paneWidth(), theme().scale), paddingLeft: indent(), paddingVertical: 2,
      backgroundColor: selectedRow() === row().id ? theme().palette.panel : "transparent" }}>
      {disclosure() ? <Header row={row} /> : <Leaf row={row} />}
      {showBody() && <Body row={row} />}
      {hasArtifacts() && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 4, paddingVertical: 4 }}>
        {artifacts().map((artifact) => {
          if (artifact.path) return control(`▤ ${artifact.label.split("/").pop()}`, () => { void openLink(`${artifact.path}${artifact.line ? `:${artifact.line}` : ""}`); });
          if (artifact.imageIndex === null || artifact.imageIndex === undefined) return null;
          imageCards += 1;
          return <ChatImage id={`${artifact.itemId}:${artifact.imageIndex}`} label={artifact.label} inline={!!artifact.inlineImage}
            preview={() => handle.imagePreview(artifact.itemId, artifact.imageIndex!)} cache={previews} theme={theme}
            width={() => paneWidth() - indent() - chatGutter(paneWidth(), theme().scale)}
            expand={(value) => imagePreview.set(value)} openLink={(url) => { void openLink(url); }} />;
        })}
      </View>}
    </View>;
  }

  function Header(headerProps: { row: () => ChatRow }) {
    const row = headerProps.row;
    const label = () => [row().title, row().target, row().failedCount && !row().title.includes("failed") ? `· ${row().failedCount} failed` : ""].filter(Boolean).join(" ");
    const activityIcon = (): IconName => row().kind === "shell" ? "terminal"
      : ["search", "web_search"].includes(row().kind) ? "search"
      : ["read", "edit"].includes(row().kind) ? "file"
      : ["worker", "subagent"].includes(row().kind) ? "person" : "tasks";
    const hasIcon = computed(() => row().kind !== "group" && row().kind !== "activity");
    const chevron = computed((): IconName => row().open ? "down" : "right");
    const kindIcon = computed(activityIcon);
    const hasTarget = computed(() => !!row().target);
    const hasMeta = computed(() => !!row().meta);
    const hasDuration = computed(() => row().durationMs !== undefined && !/^worked\s/i.test(row().title));
    const running = computed(() => row().running);
    const failed = computed(() => !row().running && row().failedCount > 0 && !row().title.includes("failed"));
    const duration = () => {
      const ms = row().durationMs ?? 0;
      return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
    };
    return <View role="button" testID={`chat-header:${row().id}`} accessibilityLabel={label()} accessibilityState={{ expanded: row().open }}
      onPress={() => { following.set(false); handle.toggle(row().id); }}
      style={{ minHeight: space(28, theme().scale), paddingVertical: 4, paddingHorizontal: 5, flexDirection: "row", alignItems: "center", gap: 7, borderRadius: 5 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Icon name={chevron()} size={11} color={theme().palette.muted} />
      {hasIcon() && <Icon name={kindIcon()} size={14} color={theme().palette.muted} />}
      <Text numberOfLines={1} style={{ ...text(12, row().running ? theme().palette.text : theme().palette.muted), flexShrink: 1 }}>{row().title}</Text>
      {hasTarget() && <Text numberOfLines={1} style={{ ...mono(11, theme().palette.muted), flexShrink: 2 }}>{row().target ?? ""}</Text>}
      {hasMeta() && <Text numberOfLines={1} style={{ ...text(11, theme().palette.muted), flexShrink: 3 }}>{`· ${row().meta ?? ""}`}</Text>}
      {hasDuration() && <Text style={text(10, theme().palette.muted)}>{duration()}</Text>}
      {running() && <Text numberOfLines={1} style={{ ...text(10, theme().palette.working), flexShrink: 1 }}>{row().currentLabel || "Running"}</Text>}
      {failed() && <Text style={text(10, ERROR)}>{`${row().failedCount} failed`}</Text>}
    </View>;
  }

  function Leaf(leafProps: { row: () => ChatRow }) {
    const row = leafProps.row;
    const kind = computed(() => row().kind);
    const attachments = computed<NonNullable<ChatRow["attachments"]>>(() => row().attachments ?? [], { equals: sameContent });
    const hasAttachments = computed(() => attachments().length > 0);
    const fileCount = computed(() => row().fileCount ?? 0);
    return () => {
      const shown = kind();
      if (shown === "user") return <View style={{ marginTop: space(14, theme().scale), marginBottom: space(10, theme().scale), padding: space(14, theme().scale), borderRadius: 10,
        borderWidth: 1, borderColor: chatHairline(theme()), backgroundColor: theme().palette.panel }}>
        <View style={{ flexDirection: "row", alignItems: "center", marginTop: -5, marginBottom: 5 }}>
          <Text style={{ ...text(11, theme().palette.muted), fontWeight: "600" }}>You</Text><View style={{ flex: 1 }} />
          {iconControl("copy", "Copy message", () => void copy(row().id))}
        </View>
        <Body row={row} />
        {hasAttachments() && <View style={{ gap: 5, marginTop: 6 }}>
          {attachments().map((attachment) => <SentAttachment attachment={attachment} />)}
        </View>}
      </View>;
      if (shown === "final" || shown === "commentary" || shown === "reasoning" || shown === "notice") return <View style={{ marginTop: space(shown === "final" ? 12 : 5, theme().scale), marginBottom: space(8, theme().scale), gap: 8 }}>
        {shown === "final" && <Text style={{ ...text(11, theme().palette.muted), fontWeight: "600" }}>{row().title || "Assistant"}</Text>}
        <Body row={row} />
        {shown === "final" && <View style={{ flexDirection: "row", alignItems: "center", gap: 4, marginLeft: -5 }}>{iconControl("copy", "Copy answer", () => void copy(row().id))}
          {fileCount() > 0 && control(`Review ${fileCount()} ${fileCount() === 1 ? "file" : "files"}`, () => reviewing.set({ scope: row().id }))}</View>}
      </View>;
      if (shown === "compaction") return <View style={{ flexDirection: "row", alignItems: "center", gap: 8, marginVertical: 6 }}>
        <View style={{ flex: 1, height: 1, backgroundColor: chatHairline(theme()) }} />
        <Text style={text(10, theme().palette.muted)}>{Settings.sentenceCase("context compacted")}</Text>
        <View style={{ flex: 1, height: 1, backgroundColor: chatHairline(theme()) }} />
      </View>;
      if (shown === "request") return <View style={{ height: space(24, theme().scale), justifyContent: "center" }}>
        <Text numberOfLines={1} style={text(11, theme().palette.muted)}>{`✓ ${row().title}${row().target ? ` · ${row().target}` : ""}`}</Text>
      </View>;
      return null;
    };
  }

  function SentAttachment(props: { attachment: ChatAttachment }) {
    const expanded = signal(false);
    const thumbnail = signal<string | null>(null);
    const attachment = props.attachment;
    function toggle() {
      expanded.set(!expanded());
      if (expanded() && attachment.preview.kind === "image" && !thumbnail()) {
        void handle.attachmentPreview(attachment).then((image) => thumbnail.set(image ?? null)).catch((error) => notice(String(error)));
      }
    }
    return <View style={{ padding: 7, borderWidth: 1, borderRadius: 6, borderColor: theme().palette.divider, gap: 5 }}>
      <View role="button" accessibilityLabel={`Preview ${attachment.name}`} accessibilityState={{ expanded: expanded() }} onPress={toggle}
        style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        <Text style={text(10, theme().palette.muted)}>{expanded() ? "▾" : "▸"}</Text>
        <Text numberOfLines={1} style={{ ...text(11), flex: 1 }}>{attachment.name}</Text>
        <Text style={text(10, theme().palette.muted)}>{`${Math.max(1, Math.round(attachment.bytes / 1024))} KB`}</Text>
      </View>
      {expanded() && attachment.preview.kind === "text" && <Text selectable={true} style={mono(11)}>{attachment.preview.excerpt}</Text>}
      {expanded() && thumbnail() && <Image src={thumbnail()!} style={{ width: 256, height: 192 }} />}
    </View>;
  }

  function Body(bodyProps: { row: () => ChatRow }) {
    const body = () => bodySignal(bodyProps.row().key)();
    // What the body is, and whose: a streamed delta changes neither, so the
    // TextView stays and takes the new blocks (it remakes only those that
    // changed). A recycled cell's new row starts its body afresh.
    const shown = computed(() => `${bodyProps.row().key}\u0000${body()?.kind ?? ""}`);
    const muted = () => bodyProps.row().kind === "commentary";
    let markdown: YNode | undefined;
    const codeActions = () => [
      { id: "copy", label: "Copy code" },
      ...(!bodyProps.row().running && !bodyProps.row().provisional ? [{ id: "reader", label: "Reader" }, { id: "save", label: "Save…" }] : []),
      ...(props.onCopyToTerminal && props.terminalTargets && !bodyProps.row().running && !bodyProps.row().provisional
        ? [{ id: "terminal", label: "Copy & open terminal…" }] : []),
    ];
    // The body of `kind`, else none: a node about to go may read it once more.
    const of = <K extends ChatBody["kind"]>(kind: K) => {
      const value = body();
      return value?.kind === kind ? value as Extract<ChatBody, { kind: K }> : undefined;
    };
    return () => {
      const kind = shown().split("\u0000")[1];
      bodyViews += 1;
      if (!kind) return <Text style={text(11, theme().palette.muted)}>…</Text>;
      if (kind === "markdown") return <TextView ref={(node) => { markdown = node; }} selectable={true} links="report" testID={`chat-body:${bodyProps.row().key}`}
        document={{ blocks: of("markdown")?.blocks ?? [] }}
        onLinkPress={(event) => void openLink(event.url)} codeBlockActions={codeActions()}
        onCodeBlockAction={(event) => {
          if (!interactive() || bodyProps.row().running || bodyProps.row().provisional || !["terminal", "reader", "save"].includes(event.id)) return;
          if (utf8Bytes(event.code.code) > MAX_SNIPPET) { notice("Reader/export limit: 256 KiB. Copy the text you need instead."); return; }
          if (event.id === "terminal" && props.onCopyToTerminal && props.terminalTargets) {
            codeReturnFocus = markdown;
            codeSnippet.set(event.code.code);
          } else if (event.id === "reader") {
            codeReturnFocus = markdown;
            codeReader.set({ snippet: { ...event.code }, directory: panel().cwd });
          } else if (event.id === "save") {
            exporting.set(true);
            void saveSnippet({ ...event.code }, panel().cwd).then((result) => { if (alive && result.saved) notice(`Saved ${result.path ?? "snippet"}`); })
              .catch((error) => { if (alive) notice(`Could not save snippet: ${String(error)}`); })
              .finally(() => { if (alive) { exporting.set(false); markdown?.focus(); } });
          }
        }}
        styles={{
          root: { ...text(muted() ? 13 : 14, muted() ? theme().palette.muted : theme().palette.text), lineHeight: (muted() ? 20 : 22) * theme().scale, gap: space(10, theme().scale) },
          heading1: { fontSize: 22 * theme().scale, lineHeight: 29 * theme().scale, marginTop: space(14, theme().scale) },
          heading2: { fontSize: 18 * theme().scale, lineHeight: 25 * theme().scale, marginTop: space(12, theme().scale) },
          heading3: { fontSize: 16 * theme().scale, lineHeight: 23 * theme().scale, marginTop: space(10, theme().scale) },
          heading4: { fontSize: 14 * theme().scale, lineHeight: 22 * theme().scale, marginTop: space(8, theme().scale) },
          code: { ...mono(12), backgroundColor: theme().palette.panel },
          codeBlock: { backgroundColor: theme().palette.panel, borderRadius: 8, borderWidth: 1, borderColor: chatHairline(theme()) },
          codeBlockCode: { ...mono(12), lineHeight: 19 * theme().scale, padding: space(12, theme().scale) },
          blockquote: { borderColor: chatHairline(theme()) },
          rule: { borderColor: chatHairline(theme()) },
        }} />;
      if (kind === "text") return <Text selectable={true} testID={`chat-body:${bodyProps.row().key}`} style={{
        ...text(bodyProps.row().kind === "user" ? 14 : 13, toneColor(of("text")?.tone ?? "")),
        lineHeight: 21 * theme().scale,
        fontStyle: of("text")?.tone === "reasoning" ? "italic" : "normal",
      }}>{of("text")?.text ?? ""}</Text>;
      if (kind === "changes") return <View style={{ paddingVertical: 5, gap: 3 }}>
        {(of("changes")?.files ?? []).map((file) => <View style={{ flexDirection: "row", alignItems: "center", gap: 7, paddingVertical: 4 }}>
          <Text style={text(10, theme().palette.muted)}>{file.kind}</Text>
          <View role="button" onPress={() => void openLink(file.path)} style={{ flex: 1, minWidth: 0 }}><Text numberOfLines={1} style={mono(11, theme().palette.cyan)}>{file.path}</Text></View>
          <Text style={text(10, theme().palette.muted)}>{`+${file.added} −${file.removed}`}</Text>
          {control("Review", () => reviewing.set({ scope: bodyProps.row().id, key: file.id }))}
        </View>)}
      </View>;
      if (kind === "tool") return <ToolResult body={() => of("tool")} />;
      if (kind === "command") return <CommandBox body={() => of("command")} />;
      if (kind === "detail") return <Scroll style={{ maxHeight: 320, padding: 8, borderRadius: 4, backgroundColor: theme().palette.panel }}>
        <Text selectable={true} style={mono(11)}>{of("detail")?.text ?? ""}</Text>
      </Scroll>;
      if (kind === "steps") return <View style={{ gap: 2, paddingVertical: 4 }}>
        {!!of("steps")?.explanation && <Text style={text(12)}>{of("steps")?.explanation ?? ""}</Text>}
        {(of("steps")?.steps ?? []).map((step) => <Text style={text(12, step.status === "completed" ? theme().palette.muted : theme().palette.text)}>
          {`${step.status === "completed" ? "✓" : step.status === "in_progress" ? "◐" : "○"} ${step.text}`}
        </Text>)}
      </View>;
      return null;
    };
  }

  function ToolResult(props: { body: () => Extract<ChatBody, { kind: "tool" }> | undefined }) {
    const raw = signal(false);
    const output = computed(() => props.body()?.view, { equals: sameContent });
    const kind = computed(() => output()?.kind);
    const omitted = computed(() => {
      const view = output();
      return view?.kind === "web_search" || view?.kind === "file_search" ? view.omitted : 0;
    });
    return <View style={{ gap: 5, paddingVertical: 4 }}>
      {kind() === "file_search" && <>
        {(output() as Extract<ReturnType<typeof output>, { kind: "file_search" }>).hits.map((hit) => <View style={{ gap: 3, paddingVertical: 3 }}>
          {control(`${hit.path}${hit.line ? `:${hit.line}` : ""}`, () => void openLink(`${hit.path}${hit.line ? `:${hit.line}` : ""}`))}
          {hit.preview && <Text selectable={true} style={mono(11, theme().palette.muted)}>{hit.preview}</Text>}
        </View>)}
      </>}
      {kind() === "web_search" && <>
        {(output() as Extract<ReturnType<typeof output>, { kind: "web_search" }>).hits.map((hit) => <View style={{ gap: 3, paddingVertical: 3 }}>
          {control(hit.title || hit.url, () => void openLink(hit.url))}
          {hit.snippet && <Text selectable={true} style={text(11, theme().palette.muted)}>{hit.snippet}</Text>}
        </View>)}
      </>}
      {kind() === "file_read" && control((output() as Extract<ReturnType<typeof output>, { kind: "file_read" }>).path, () => void openLink((output() as Extract<ReturnType<typeof output>, { kind: "file_read" }>).path))}
      {(kind() === "file_read" || kind() === "json") && <Scroll style={{ maxHeight: 300, padding: 8, borderRadius: 4, backgroundColor: theme().palette.panel }}>
        <Text selectable={true} style={mono(11)}>{(output() as Extract<ReturnType<typeof output>, { kind: "file_read" | "json" }> | undefined)?.text ?? ""}</Text>
        {(output() as Extract<ReturnType<typeof output>, { kind: "file_read" | "json" }>).truncated && <Text style={text(10, theme().palette.muted)}>Preview truncated</Text>}
      </Scroll>}
      {omitted() > 0 && <Text style={text(10, theme().palette.muted)}>{`${omitted()} more results in raw output`}</Text>}
      <View style={{ flexDirection: "row" }}>{control(raw() ? "Hide raw output" : "Raw output", () => raw.set(!raw()))}{control("Copy output", () => void Clipboard.writeText(props.body()?.raw ?? ""))}</View>
      {raw() && <Scroll style={{ maxHeight: 300, padding: 8, backgroundColor: theme().palette.panel }}><Text selectable={true} style={mono(11)}>{props.body()?.raw ?? ""}</Text></Scroll>}
    </View>;
  }

  // Output streams into a running command: its text updates in place, and the
  // lines around it come and go only when they do.
  function CommandBox(boxProps: { body: () => Extract<ChatBody, { kind: "command" }> | undefined }) {
    const body = boxProps.body;
    const hasCwd = computed(() => !!body()?.cwd);
    const hasExit = computed(() => body()?.exitCode !== undefined);
    const hasOmitted = computed(() => (body()?.omitted ?? 0) > 0);
    return <View style={{ gap: 2, paddingVertical: 2 }}>
      <View style={{ flexDirection: "row", alignItems: "center" }}>
        <Text selectable={true} style={{ ...mono(11, theme().palette.muted), flex: 1 }}>{`$ ${body()?.command ?? ""}`}</Text>
        {control("Copy output", () => void Clipboard.writeText(body()?.output ?? ""))}
      </View>
      {hasCwd() && <Text selectable={true} style={mono(10, theme().palette.muted)}>{body()?.cwd ?? ""}</Text>}
      {hasExit() && <Text style={mono(10, body()?.exitCode === 0 ? theme().palette.muted : ERROR)}>{`Exit ${body()?.exitCode}`}</Text>}
      <Scroll style={{ maxHeight: 320, padding: 8, borderRadius: 4, backgroundColor: theme().palette.panel }}>
        {hasOmitted() && <Text style={{ ...mono(10, theme().palette.muted), fontStyle: "italic" }}>{`${body()?.omitted ?? 0} earlier bytes not shown`}</Text>}
        <Text selectable={true} style={mono(11)}>{body()?.output || (body()?.running ? "Waiting for output…" : "")}</Text>
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
    const hasStatus = computed(() => !!panel().status);
    const welcome = computed(() => !panel().status && panel().empty && panel().link === "live");
    const hasCwd = computed(() => !!panel().cwd);
    return <View testID="chat-footer" style={{ ...chatColumn(paneWidth(), theme().scale), paddingTop: 12, paddingBottom: 24, gap: 10 }}>
      {hasStatus() && <View style={{ flexDirection: "row", alignItems: "center", gap: 7 }}>
        <View accessible={false} style={{ width: 5, height: 5, borderRadius: 3, backgroundColor: panel().state === "waiting" ? theme().palette.gold : theme().palette.working }} />
        <Text testID="chat-status" style={{ ...text(11, panel().state === "waiting" ? theme().palette.gold : theme().palette.working), flex: 1 }}>{panel().status ?? ""}</Text>
      </View>}
      {welcome() && <>
        <View style={{ alignItems: "flex-start", paddingTop: space(28, theme().scale), gap: 10 }}>
          <Icon name="chat" size={25} color={theme().palette.muted} />
          <Text style={{ ...text(19), fontWeight: "600" }}>Start a conversation</Text>
          <Text style={{ ...text(13, theme().palette.muted), lineHeight: 20 * theme().scale }}>Ask a question, plan a change, or describe what you want to build.</Text>
          {hasCwd() && <View style={{ flexDirection: "row", alignItems: "center", gap: 6, marginTop: 4 }}>
            <Icon name="folder" size={14} color={theme().palette.muted} />
            <Text selectable={true} numberOfLines={2} style={{ ...mono(11, theme().palette.muted), flexShrink: 1 }}>{panel().cwd}</Text>
          </View>}
          <Text style={{ ...text(11, theme().palette.muted), marginTop: 8 }}>Use / for commands, @ for files, or drop an attachment.</Text>
        </View>
      </>}
    </View>;
  }

  // A boolean, so a panel delivery never rebuilds the transcript and composer.
  const deleted = computed(() => panel().link === "deleted");
  // The fragment's expressions below run again together, whenever one of
  // them changes: each reads a memo, so a streaming delivery runs none.
  const jumpToLatest = computed(() => !following() && prompts().length < 2);
  const banners = computed(() => panel().notices, { equals: sameContent });
  const stopped = computed(() => panel().state === "stopped");
  const failed = computed(() => panel().state === "failed");
  return <View testID={`chat:${props.chatId}`} onLayout={(bounds) => paneWidth.set(bounds.size.width)} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.bg }}>
    {deleted() ? <View testID="chat-deleted" style={{ flex: 1, padding: 24, gap: 8 }}>
      <Text style={text(14)}>This chat was deleted</Text>
      <Text style={text(12, theme().palette.muted)}>Its history is gone from the chat host.</Text>
      {props.onClose && <View role="button" accessibilityLabel="Close" onPress={props.onClose}><Text style={text(12, theme().palette.cyan)}>Close</Text></View>}
    </View> : <>
      <ChatHeader />
      <List testID="chat-transcript" ref={(node) => { list = node; }} style={{ flex: 1, minHeight: 0 }}
        each={rows()} keyExtractor={(row) => row.key} estimatedItemSize={space(28, theme().scale)}
        maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 48 }}
        onScrollDragStart={() => { following.set(false); }}
        onViewableItemsChange={(info) => {
          const range = viewport(info.viewableItems.map((token) => token.index));
          if (range) {
            handle.setViewport(range[0], range[1]); firstVisible.set(range[0]);
            if (range[1] >= rows().length - 1) following.set(true);
          }
          report();
        }}
        footer={() => <Footer />}>
        {(row) => <Row row={row} />}
      </List>
      {jumpToLatest() && <View style={{ alignItems: "center" }}>{control("Jump to latest ↓", latest)}</View>}
      <WorkerShelf handle={handle} panel={panel} visible={() => props.visible} theme={theme} width={paneWidth} onOpen={props.onOpenWorker} onError={notice}
        onReveal={(id) => {
          following.set(false); selectedRow.set(id); pendingReveal = id;
          void handle.revealItem(id).then((index) => { if (index !== undefined && index !== null) jumpTo(index); }).catch((error) => notice(String(error)));
        }} />
      <Requests panel={panel} handle={handle} theme={theme} notice={notice} width={paneWidth} />
      {notices().length > 0 && <View testID="chat-notices" style={{ ...chatColumn(paneWidth(), theme().scale), gap: 2 }}>
        {notices().map((message, index) => <View style={{ flexDirection: "row", gap: 8, paddingVertical: 4 }}>
          <Text testID="chat-notice" style={{ ...text(11, theme().palette.gold), flex: 1 }}>{message}</Text>
          {iconControl("close", "Dismiss notice", () => notices.set(notices().filter((_, at) => at !== index)))}
        </View>)}
      </View>}
      {banners().map((banner) => <View testID={`chat-banner:${banner.id}`} style={{ ...chatColumn(paneWidth(), theme().scale), paddingVertical: 4 }}>
        <Text style={text(11, banner.level === "error" ? ERROR : banner.level === "warning" ? theme().palette.gold : theme().palette.muted)}>{banner.text}</Text>
        <BillingNoticeAction control={billing} noticeId={banner.id} theme={theme} />
      </View>)}
      {stopped() && <Text style={{ ...text(11, theme().palette.muted), ...chatColumn(paneWidth(), theme().scale) }}>Stopped. The chat will resume when you send a message.</Text>}
      {failed() && <Text style={{ ...text(11, theme().palette.gold), ...chatColumn(paneWidth(), theme().scale) }}>{`Failed: ${panel().stateMessage ?? ""}`}</Text>}
      <Composer panel={panel} handle={handle} theme={theme} notice={notice} visible={interactive} width={paneWidth} billing={billing}
        onSent={() => { following.set(true); list?.scrollToEnd(); }} onEditor={(editor) => editors.push(editor)} onEditMemo={props.onEditMemo}
        onMeasure={(part, height) => { composer[part] = height; report(); }} />
    </>}
    {codeSnippet() !== null && props.terminalTargets && props.onCopyToTerminal && <CodeTerminalDialog text={codeSnippet()!} targets={props.terminalTargets}
      onCopy={async (id, text) => { await props.onCopyToTerminal!(id, text); codeReturnFocus = undefined; }} theme={theme} onClose={closeCodeSnippet} />}
    {codeReader() && <CodeReader snippet={codeReader()!.snippet} directory={codeReader()!.directory} theme={theme}
      onClose={() => { codeReader.set(null); const restore = codeReturnFocus; codeReturnFocus = undefined; restore?.focus(); }} />}
    {reviewing() && <FileReview handle={handle} scope={() => reviewing()?.scope} initial={() => reviewing()?.key} theme={theme} active={active}
      onClose={() => reviewing.set(null)} onOpen={(path, line) => void openLink(`${path}${line ? `:${line}` : ""}`)} onError={notice} />}
    {imagePreview() && <View style={{ position: "absolute", left: 0, right: 0, top: 0, bottom: 0, zIndex: 21, backgroundColor: theme().palette.bg, padding: 12, gap: 8 }}>
      <View style={{ flexDirection: "row", alignItems: "center" }}><Text numberOfLines={1} style={{ ...text(13), flex: 1 }}>{imagePreview()?.label || "Image preview"}</Text>{control("Close", closeImage)}</View>
      {imagePreview()?.dataUrl && <Scroll style={{ flex: 1 }}><Image src={imagePreview()!.dataUrl!} resizeMode="contain" style={{ width: "100%", height: Math.min(640, paneWidth() * ((imagePreview()!.height || 3) / (imagePreview()!.width || 4))) }}
        onError={(event) => imagePreview.set({ ...imagePreview()!, dataUrl: null, message: event.message || "This image could not be displayed" })} /></Scroll>}
      {imagePreview()?.url && control("Open image", () => void openLink(imagePreview()!.url!))}
      {imagePreview()?.message && <Text style={text(12, theme().palette.muted)}>{imagePreview()!.message}</Text>}
    </View>}
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
function Requests(props: PartProps & { width: () => number }) {
  // What a mounted request shows. It keeps the last request through the
  // delivery that ends it: its own expressions may read it before the gate,
  // keyed by request id, takes it away.
  const approval = computed<ChatApproval | undefined>((last) => props.panel().approvals[0] ?? last, { equals: sameContent });
  const question = computed<ChatQuestion | undefined>((last) => props.panel().questions[0] ?? last, { equals: sameContent });
  const more = computed(() => props.panel().approvals.length - 1);
  const approvalId = computed(() => props.panel().approvals[0]?.requestId);
  const questionId = computed(() => props.panel().questions[0]?.requestId);
  return <View style={chatColumn(props.width(), props.theme().scale)}>
    <View>{approvalId() !== undefined && <ApprovalBar {...props} approval={() => approval()!} more={more} />}</View>
    <View>{questionId() !== undefined && <QuestionPanel {...props} question={() => question()!} />}</View>
  </View>;
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
  return <View testID="chat-approval" accessibilityLabel={props.approval().heading} style={{ marginBottom: 6, padding: 12, gap: 7, borderRadius: 8, borderWidth: 1, borderColor: chatHairline(theme()), backgroundColor: theme().palette.panel }}>
    <View style={{ flexDirection: "row", gap: 8 }}>
      <Text style={{ ...text(12, theme().palette.gold), fontWeight: "600", flexShrink: 1 }}>{props.approval().heading}</Text>
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
    }).catch((error) => { sent.set(false); props.notice(`Could not send answers: ${error}`); });
  };
  return <View testID="chat-question" style={{ marginBottom: 6, padding: 12, gap: 9, borderRadius: 8, borderWidth: 1, borderColor: chatHairline(theme()), backgroundColor: theme().palette.panel }}>
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

interface ComposerAttachment {
  key: string;
  name: string;
  attachment?: ChatAttachment;
  uri?: string;
  file?: File;
  error?: string;
}

function encodeAttachment(bytes: Uint8Array): string {
  // A multiple of three keeps the intermediate chunks free of padding.
  const chunks: string[] = [];
  for (let at = 0; at < bytes.length; at += 24_576) chunks.push(btoa(String.fromCharCode(...bytes.subarray(at, at + 24_576))));
  return chunks.join("");
}

/** The message box, its send/stop slot and the model, effort, Fast and mode
 * choices. Drafts are saved on every change and restored into an empty box. */
function Composer(props: PartProps & { billing: BillingControl; width: () => number; visible: () => boolean; onSent: () => void; onEditor: (editor: number) => void; onEditMemo?: (projectId: string) => void;
  onMeasure?: (part: "box" | "input" | "controls", height: number) => void }) {
  const theme = props.theme;
  // One draft editor identity per mounted composer, unique across windows: the
  // native draft owners are process-wide (CHATC-035).
  const editor = Chat.newEditor();
  props.onEditor(editor);
  let generation = 0;
  const draft = signal("");
  const memo = createMemoPreview();
  const attachments = signal<ComposerAttachment[]>([]);
  const previewing = signal<string | null>(null);
  const thumbnail = signal<string | null>(null);
  const draggingFiles = signal(false);
  let attachmentSequence = 0;
  let mounted = true;
  cleanup(() => { mounted = false; });
  const readyAttachments = () => attachments().flatMap((entry) => entry.attachment ? [entry.attachment] : []);
  const incompleteAttachments = () => attachments().some((entry) => !entry.attachment);
  const pending = signal(false);
  const composing = signal(false);
  const dictation = signal<DictationController | undefined>(undefined);
  const selection = signal({ start: 0, end: 0 });
  const dictationEnabled = signal(false), holdKey = signal("off");
  const dictationSettings = Settings.subscribe((event) => { dictationEnabled.set(event.data.dictationMic); holdKey.set(event.data.pushToTalkKey); });
  cleanup(() => dictationSettings.close());
  const focused = signal(false);
  const modelPicker = signal(false);
  const modelQuery = signal("");
  const contentWidth = () => Math.min(props.width() || CHAT_COLUMN_WIDTH, CHAT_COLUMN_WIDTH) - 2 * chatGutter(props.width(), theme().scale) - 24;
  const roomy = () => contentWidth() >= 620 * theme().scale;
  const spacious = () => contentWidth() >= 760 * theme().scale;
  const saved = signal<ChatDraft["submissions"]>([]);
  // The message box is as tall as its text (`onContentSizeChange`), from three
  // lines to nine. Left to measure itself under a `maxHeight`, it stops at nine
  // lines but the composer around it is sized from its whole text: Taffy sizes
  // that column from flex bases, which `maxHeight` does not clamp, and the
  // extra height opened below the controls.
  const textHeight = signal(0);
  const inputHeight = () => Math.min(9 * 21 * theme().scale, Math.max(66 * theme().scale, textHeight() + 8));
  let lastSend = 0;
  let input: YNode | undefined;
  const text = (base = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  const toolbar = () => props.panel().toolbar;
  const empty = () => draft().trim() === "" && attachments().length === 0;
  const accepting = () => !props.billing.pending() && props.panel().link !== "deleted" && props.panel().link !== "connecting";
  const approval = () => props.panel().approvals[0];
  const guarded = () => Date.now() - lastSend < APPROVAL_GUARD_MS;
  const suggestions = createSuggestions({
    handle: props.handle, value: draft, selection, focused, composing,
    basis: () => `${toolbar()?.providerLabel ?? ""}\u0000${toolbar()?.modelLabel ?? ""}`,
    active: () => props.visible() && accepting() && !pending() && !memo.preview() && !modelPicker() && !dictation()?.active(),
    replace: (value, nextSelection) => {
      edit(value);
      selection.set(nextSelection);
      input?.setSelection(nextSelection.start, nextSelection.end);
    },
    focus: () => input?.focus(),
  });

  // Restore the saved draft only into an empty composer (CHATC-052).
  void props.handle.restoreDraft().then((restored) => {
    if (!restored) return;
    if (!mounted) return;
    if (draft() === "" && attachments().length === 0 && generation === 0) {
      draft.set(restored.text);
      attachments.set((restored.attachments ?? []).map((attachment) => ({ key: attachment.id, name: attachment.name, attachment })));
      if (restored.text || attachments().length) generation += 1;
    }
    saved.set(restored.submissions.filter((submission) => submission.status === "refused" || submission.status === "uncertain"));
    for (const warning of restored.warnings) props.notice(warning);
  }).catch(() => {});

  function edit(value: string) {
    dictation()?.userEdited();
    draft.set(value);
    generation += 1;
    persist();
  }

  function replaceDraft(value: string) {
    edit(value);
    selection.set({ start: value.length, end: value.length });
    input?.setSelection(value.length, value.length);
  }

  function closeMemo() { memo.close(); if (props.visible()) input?.focus(); }

  function insertMemo() {
    const preview = memo.preview();
    if (!props.visible() || !accepting() || preview?.kind !== "memo") return;
    if (preview.projectId !== props.panel().projectId) { closeMemo(); return; }
    if (preview.text.trim()) replaceDraft(appendMemo(draft(), preview.text));
    closeMemo();
  }

  function persist() {
    props.handle.saveAttachments(draft(), readyAttachments(), editor, generation, attachments().filter((entry) => !entry.attachment).length);
  }

  function attachmentChanged(next: ComposerAttachment[]) {
    attachments.set(next); generation += 1; persist();
  }

  async function stage(entry: ComposerAttachment) {
    try {
      let attachment: ChatAttachment;
      if (entry.uri) attachment = await props.handle.stageAttachment(entry.uri);
      else if (entry.file) {
        if (entry.file.size > 64 * 1024 * 1024) throw new Error("Clipboard image exceeds 64 MiB");
        const bytes = new Uint8Array(await entry.file.arrayBuffer());
        attachment = await props.handle.stageImage(entry.name, encodeAttachment(bytes));
      } else throw new Error("Choose the attachment again.");
      if (mounted && attachments().some((item) => item.key === entry.key)) {
        attachmentChanged(attachments().map((item) => item.key === entry.key ? { key: entry.key, name: attachment.name, attachment } : item));
      }
    } catch (error) {
      if (mounted && attachments().some((item) => item.key === entry.key)) {
        attachmentChanged(attachments().map((item) => item.key === entry.key ? { ...item, error: String(error) } : item));
      }
    }
  }

  function addAttachments(uris: string[], files: File[] = []) {
    if (!accepting()) return;
    const sources: ComposerAttachment[] = [
      ...uris.map((uri) => {
        let name = uri.split("/").pop() || "File";
        try { name = decodeURIComponent(name); } catch {}
        return { key: `${editor}:${++attachmentSequence}`, name, uri };
      }),
      ...files.map((file) => ({ key: `${editor}:${++attachmentSequence}`, name: file.name || "Clipboard image.png", file })),
    ];
    const admitted = sources.slice(0, Math.max(0, 8 - attachments().length));
    if (admitted.length < sources.length) props.notice("A message can include up to 8 attachments. Remove one before adding more.");
    if (!admitted.length) return;
    attachmentChanged([...attachments(), ...admitted]);
    // Clipboard bytes are read and normalized one at a time; the remaining
    // Files keep their native storage while the host takes each snapshot.
    void (async () => { for (const entry of admitted) { if (!mounted) break; await stage(entry); } })();
  }

  async function chooseAttachments() {
    try {
      const result = await host.call("system.pickChatAttachments", {}) as { paths?: string[] };
      if (mounted) addAttachments(result.paths ?? []);
    } catch (error) { props.notice(`Could not choose attachments: ${error}`); }
  }

  function previewAttachment(entry: ComposerAttachment) {
    if (previewing() === entry.key) { previewing.set(null); return; }
    previewing.set(entry.key); thumbnail.set(null);
    if (entry.attachment?.preview.kind === "image") {
      void props.handle.attachmentPreview(entry.attachment).then((image) => {
        if (mounted && previewing() === entry.key) thumbnail.set(image ?? null);
      }).catch((error) => props.notice(String(error)));
    }
  }

  function restoreSubmission(submission: ChatDraft["submissions"][number]) {
    if (pending()) return;
    draft.set(submission.text);
    attachmentChanged((submission.attachments ?? []).map((attachment) => ({ key: attachment.id, name: attachment.name, attachment })));
    input?.focus();
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
    if (composing() || !props.visible() || !accepting()) return;
    // A preview is never an implicit send or an empty-box approval shortcut.
    if (memo.preview()) return;
    if (dictation()?.finishBeforeSend(submit)) return;
    if (pending()) return;
    const rest = memoCommandRest(draft());
    if (rest !== null) {
      replaceDraft(rest);
      modelPicker.set(false);
      memo.open(props.panel().projectId);
      return;
    }
    if (empty()) {
      // ⏎ in an empty box answers a waiting approval with Allow (CHATT-207).
      const request = approval();
      if (request && !guarded()) answer(decisionForKey("enter", request.choices.map((choice) => choice.decision)));
      return;
    }
    if (pending() || !accepting() || incompleteAttachments()) return;
    const sending = draft();
    const sentAttachments = readyAttachments();
    const sentGeneration = generation;
    pending.set(true);
    lastSend = Date.now();
    props.onSent();
    void props.handle.submit({ text: sending, attachments: sentAttachments, editor, generation: sentGeneration }).then((receipt) => {
      if (receipt.status === "submitted") {
        // A newer edit (another generation) is kept (CHATC-035).
        if (generation === sentGeneration) { draft.set(""); attachments.set([]); previewing.set(null); generation += 1; }
        else persist();
        return;
      }
      persist();
      props.notice(receipt.status === "refused"
        ? `Submission refused: ${receipt.error ?? ""}. Your draft is retained.`
        : `Submission outcome uncertain: ${receipt.error ?? ""}. Inspect the transcript before explicitly resending. Your draft is retained.`);
      if (receipt.key) saved.set([...saved(), { key: receipt.key, text: sending, attachments: sentAttachments, status: receipt.status, error: receipt.error }]);
    }).catch((error) => {
      persist();
      props.notice(`Could not send: ${error}. Your draft is retained.`);
    }).finally(() => pending.set(false));
  }

  function interrupt() {
    void props.handle.interrupt().catch((error) => props.notice(`Could not reach the chat: ${error}`));
  }

  // ⎋ in the focused, empty box denies a waiting approval (CHATT-207).
  effect(() => {
    const request = approval();
    if (!props.visible() || !focused() || !empty() || !request || composing() || dictation()?.active() || memo.preview()) return;
    return keymap.bind("escape", () => {
      if (!guarded()) answer(decisionForKey("escape", request.choices.map((choice) => choice.decision)));
    }, { when: "always" });
  });

  effect(() => {
    if (!props.visible() || !memo.preview() || composing()) return;
    return keymap.bind("escape", closeMemo, { when: "always" });
  });

  const configure = (request: Parameters<ChatHandle["configure"]>[0]) =>
    void props.handle.configure(request).catch((error) => props.notice(`Could not reach the chat: ${error}`)).finally(() => input?.focus());

  function modelMenu() { modelQuery.set(""); modelPicker.set(!modelPicker()); }

  function chooseModel(model: string) {
    const name = model.trim();
    if (!name) return;
    configure({ model: name }); modelPicker.set(false);
  }

  const modelOptions = () => {
    const bar = toolbar();
    if (!bar) return [];
    const choices = bar.models.length ? bar.models : bar.modelSuggestions.map((name) => ({ id: name, label: name, detail: undefined, current: bar.model === name }));
    const query = modelQuery().trim().toLowerCase();
    return choices.filter((model) => !query || `${model.id} ${model.label} ${model.detail ?? ""}`.toLowerCase().includes(query));
  };

  function providerMenu() {
    const bar = toolbar();
    if (!bar) return;
    void Menu.popup(bar.providers.map((provider) => ({ label: `Switch to ${provider.label}`, disabled: props.panel().running, onSelect: () => {
      void props.handle.switchProvider(provider.id, null).catch((error) => props.notice(String(error)));
    } })));
  }

  function effortMenu() {
    const bar = toolbar();
    if (!bar) return;
    void Menu.popup(bar.efforts.map((effort) => ({ label: effort.label, checked: effort.current, onSelect: () => configure({ effort: effort.id }) })));
  }

  function modeMenu() {
    const bar = toolbar();
    if (!bar) return;
    void Menu.popup(bar.modes.map((mode) => ({ label: `${mode.label}${mode.detail ? ` · ${mode.detail}` : ""}`, checked: mode.current, onSelect: () => configure({ approvalMode: mode.id as "supervised" }) })));
  }

  function optionsMenu() {
    const bar = toolbar();
    const items: MenuItem[] = [];
    if (bar) {
      items.push({ label: `Provider · ${bar.providerLabel}`, submenu: bar.providers.map((provider) => ({
        label: provider.label, disabled: props.panel().running, onSelect: () => {
          void props.handle.switchProvider(provider.id, null).catch((error) => props.notice(String(error)));
        },
      })) });
      items.push({ label: `Model · ${bar.modelLabel}`, onSelect: modelMenu });
      if (bar.effortAvailable) items.push({ label: `Reasoning · ${bar.effortLabel}`, submenu: bar.efforts.map((effort) => ({ label: effort.label, checked: effort.current, onSelect: () => configure({ effort: effort.id }) })) });
      if (bar.fastAvailable) items.push({ label: "Fast mode", checked: bar.fast, onSelect: () => configure({ fast: !bar.fast }) });
      items.push({ type: "separator" });
      items.push({ label: `Approval mode · ${bar.modeLabel}`, submenu: bar.modes.map((mode) => ({ label: `${mode.label}${mode.detail ? ` · ${mode.detail}` : ""}`, checked: mode.current, onSelect: () => configure({ approvalMode: mode.id as "supervised" }) })) });
      if (bar.contributorLabel) items.push({ label: bar.contributorLabel, submenu: [
        { label: "Contributor off", checked: bar.contributorMode === "off", onSelect: () => configure({ contributorMode: "off" }) },
        { label: "Contributor on", checked: bar.contributorMode === "on", onSelect: () => configure({ contributorMode: "on" }) },
      ] });
    }
    if (props.billing.available()) {
      if (items.length) items.push({ type: "separator" });
      items.push({ label: props.billing.pending() ? "Billing · Switching…" : props.billing.summary().label, checked: props.billing.summary().on,
        disabled: props.billing.disabled(), onSelect: () => props.billing.toggle() });
    }
    if (items.length) void Menu.popup(items);
  }

  const optionsHint = () => [
    "Chat options", toolbar()?.effortAvailable ? toolbar()?.effortLabel : undefined,
    toolbar()?.fastAvailable && toolbar()?.fast ? "Fast" : undefined, toolbar()?.modeLabel,
    toolbar()?.contributorLabel, props.billing.available() ? props.billing.summary().label : undefined,
  ].filter(Boolean).join(" · ");

  // The label is read in place: a panel delivery updates it, not the pill.
  const pill = (testID: string, label: () => string, onPress: () => void, tint = theme().palette.text) =>
    <View role="button" testID={testID} focusable={true} tooltip={label()} accessibilityLabel={label()} onPress={onPress}
      style={{ height: 28 * theme().scale, minWidth: 0, flexShrink: 1, paddingHorizontal: 7 * theme().scale, justifyContent: "center", borderRadius: 5 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text numberOfLines={1} style={text(11, tint)}>{label()}</Text>
    </View>;

  // Every delivery is a new panel and toolbar: the controls below are chosen
  // by these memos, so a streaming turn keeps the same buttons.
  const hasToolbar = computed(() => !!toolbar());
  const effortAvailable = computed(() => !!toolbar()?.effortAvailable);
  const options = computed(() => modelOptions(), { equals: sameContent });
  const stopping = computed(() => props.panel().running && (empty() || pending() || incompleteAttachments()));
  const sendDisabled = () => !!memo.preview() || empty() || pending() || incompleteAttachments() || !accepting();
  return <View style={{ ...chatColumn(props.width(), theme().scale), minWidth: 0, flexShrink: 0, paddingTop: 8, paddingBottom: 12 }}>
    <View testID="chat-composer" onLayout={(bounds) => props.onMeasure?.("box", bounds.size.height)}
    onFileDragEnter={() => draggingFiles.set(true)} onFileDragLeave={() => draggingFiles.set(false)}
    onFileDrop={(event) => { draggingFiles.set(false); addAttachments(event.uris); }} style={{ minWidth: 0, padding: 10, gap: 8, borderRadius: 10, borderWidth: 1, borderColor: draggingFiles() ? theme().palette.cyan : theme().palette.divider, backgroundColor: theme().palette.panel }}>
    {memo.preview() && <MemoPreview preview={memo.preview} theme={theme} active={() => props.visible() && accepting()} onInsert={insertMemo} onCancel={closeMemo}
      onEdit={props.onEditMemo ? (projectId) => { memo.close(); props.onEditMemo?.(projectId); } : undefined} />}
    {saved().map((submission) => <View testID={`chat-saved:${submission.key}`} style={{ gap: 2, paddingBottom: 4 }}>
      <Text style={text(11, theme().palette.gold)}>{submission.status === "refused" ? `Submission refused${submission.error ? `: ${submission.error}` : ""}` : `Submission outcome uncertain${submission.error ? `: ${submission.error}` : ""}`}</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 12 }}>
        <View role="button" accessibilityLabel="Replace current draft with saved" onPress={() => restoreSubmission(submission)}><Text style={text(11, theme().palette.cyan)}>Replace current draft with saved</Text></View>
        <View role="button" accessibilityLabel="Dismiss saved submission" onPress={() => { props.handle.dismissSubmission(submission.key); saved.set(saved().filter((other) => other.key !== submission.key)); }}><Text style={text(11, theme().palette.cyan)}>Dismiss saved submission</Text></View>
      </View>
    </View>)}
    {draggingFiles() && <Text style={text(12, theme().palette.cyan)}>Drop files to attach</Text>}
    {attachments().length > 0 && <View style={{ gap: 5 }}>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 5 }}>
        {attachments().map((entry) => <View style={{ maxWidth: 240, minWidth: 100, flexShrink: 1, borderWidth: 1, borderRadius: 6,
          borderColor: entry.error ? ERROR : previewing() === entry.key ? theme().palette.cyan : theme().palette.divider, padding: 6, gap: 3 }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 5 }}>
            <View role="button" focusable={true} accessibilityLabel={`Preview ${entry.name}`} onPress={() => previewAttachment(entry)} style={{ flex: 1, minWidth: 0, flexDirection: "row", alignItems: "center", gap: 5, borderRadius: 3 }}
              focusStyle={{ backgroundColor: theme().palette.panelActive }}>
              <Icon name={entry.attachment?.kind.kind === "image" ? "eye" : "file"} size={13 * theme().scale} color={theme().palette.muted} />
              <Text numberOfLines={1} style={{ ...text(11), minWidth: 0, flexShrink: 1 }}>{entry.name}</Text>
            </View>
            <View role="button" focusable={true} accessibilityLabel={`Remove ${entry.name}`} onPress={() => {
              if (previewing() === entry.key) previewing.set(null);
              attachmentChanged(attachments().filter((item) => item.key !== entry.key));
            }} style={{ width: 20, height: 20, flexShrink: 0, borderRadius: 4, alignItems: "center", justifyContent: "center" }}
              hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name="close" size={10 * theme().scale} color={theme().palette.muted} /></View>
          </View>
          <Text numberOfLines={2} style={text(10, entry.error ? ERROR : theme().palette.muted)}>{entry.error ?? (entry.attachment ? `${Math.max(1, Math.round(entry.attachment.bytes / 1024))} KB · Ready` : "Preparing…")}</Text>
          {entry.error && <View role="button" accessibilityLabel={`Retry ${entry.name}`} onPress={() => {
            attachmentChanged(attachments().map((item) => item.key === entry.key ? { ...item, error: undefined } : item)); void stage(entry);
          }}><Text style={text(10, theme().palette.cyan)}>Retry</Text></View>}
        </View>)}
      </View>
      {previewing() && <View style={{ padding: 7, borderRadius: 5, backgroundColor: theme().palette.bg }}>
        {thumbnail() && <Image src={thumbnail()!} style={{ width: "100%", maxWidth: 192, height: 128 }} />}
        {attachments().find((entry) => entry.key === previewing())?.attachment?.preview.kind === "text" && <Scroll style={{ maxHeight: 130 }}>
          <Text selectable={true} style={{ ...text(11, theme().palette.muted), fontFamily: theme().monoFamily }}>{(attachments().find((entry) => entry.key === previewing())!.attachment!.preview as { kind: "text"; excerpt: string }).excerpt}</Text>
        </Scroll>}
      </View>}
    </View>}
    {modelPicker() && <View style={{ minWidth: 0, gap: 6, padding: 4, paddingBottom: 10, borderBottomWidth: 1, borderColor: chatHairline(theme()) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        <Input ref={(node) => node.focus()} value={modelQuery()} onChange={(value) => modelQuery.set(value)} onSubmit={() => {
          const first = modelOptions()[0];
          if (first) chooseModel(first.id);
          else if (!toolbar()?.models.length) chooseModel(modelQuery());
        }}
          placeholder="Search models or enter a model name" accessibilityLabel="Search or enter model" style={{ ...text(12), flex: 1, minWidth: 0, padding: 7, borderWidth: 1, borderColor: theme().palette.divider, borderRadius: 5 }} />
        <View role="button" testID="chat-model-close" focusable={true} tooltip="Close model picker" accessibilityLabel="Close model picker" onPress={() => { modelPicker.set(false); input?.focus(); }}
          style={{ width: 28 * theme().scale, height: 28 * theme().scale, flexShrink: 0, borderRadius: 5, alignItems: "center", justifyContent: "center" }}
          hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name="close" size={12 * theme().scale} color={theme().palette.muted} /></View>
      </View>
      <Scroll style={{ maxHeight: 200 }}>
        {options().map((model) => <View role="button" accessibilityLabel={model.label} onPress={() => chooseModel(model.id)}
          style={{ padding: 7, gap: 3, borderRadius: 4, backgroundColor: model.current ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
          <Text style={text(12)}>{`${model.current ? "✓ " : ""}${model.label}`}</Text>
          {model.detail && <Text numberOfLines={3} style={text(11, theme().palette.muted)}>{model.detail}</Text>}
        </View>)}
      </Scroll>
      {modelQuery().trim() && !options().some((model) => model.id === modelQuery().trim()) && pill("chat-model-custom", () => `Use “${modelQuery().trim()}”`, () => chooseModel(modelQuery()))}
      {!options().length && !modelQuery().trim() && <Text style={text(11, theme().palette.muted)}>Enter the model name supplied by your provider.</Text>}
    </View>}
    <DictationStatus controller={dictation} theme={theme} />
    <Suggestions control={suggestions} theme={theme} />
    <Input ref={(node) => { input = node; }} testID="chat-input" accessibilityLabel="Chat message" placeholder={props.panel().running ? "Send a follow-up to steer this turn…" : "Write a message…"}
      multiline={true} submitBehavior="submit" readonly={!accepting()} value={draft()}
      onSelectionChange={(event) => selection.set({ start: event.start, end: event.end })}
      onPaste={(event) => addAttachments(event.uris, event.uris.length ? [] : event.files)}
      onChange={edit} onSubmit={() => { if (!suggestions.accept()) submit(); }} onFocus={() => focused.set(true)} onBlur={() => focused.set(false)}
      onCompositionChange={(event) => composing.set(event.composing)} onContentSizeChange={(size) => textHeight.set(size.height)}
      onLayout={(bounds) => props.onMeasure?.("input", bounds.size.height)}
      style={{ ...text(14), lineHeight: 21 * theme().scale, height: inputHeight(), paddingHorizontal: 3, paddingVertical: 4 }} />
    <View onLayout={(bounds) => props.onMeasure?.("controls", bounds.size.height)}
      style={{ flexDirection: "row", alignItems: "center", minWidth: 0, gap: 3, paddingTop: 7, borderTopWidth: 1, borderColor: chatHairline(theme()) }}>
      <View role="button" testID="chat-attach" focusable={accepting()} tooltip="Attach files · You can also paste or drop images and files" accessibilityLabel="Attach files"
        accessibilityState={{ disabled: !accepting() }} onPress={() => { if (accepting()) void chooseAttachments(); }}
        style={{ width: 28 * theme().scale, height: 28 * theme().scale, flexShrink: 0, borderRadius: 5, alignItems: "center", justifyContent: "center", opacity: accepting() ? 1 : 0.4 }}
        hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
        <Icon name="add" size={16 * theme().scale} color={theme().palette.muted} />
      </View>
      {spacious() && hasToolbar() && <View style={{ maxWidth: 90 * theme().scale, minWidth: 0, flexShrink: 1 }}>
        {pill("chat-provider", () => toolbar()?.providerLabel ?? "", providerMenu, theme().palette.muted)}
      </View>}
      <View style={{ flex: 1, minWidth: 0 }}>
        {hasToolbar() && <View role="button" testID="chat-model" focusable={true} tooltip={`${toolbar()?.providerLabel ?? ""} · ${toolbar()?.modelLabel ?? ""} · Choose model`}
          accessibilityLabel={`Choose model, ${toolbar()?.modelLabel ?? ""}`} onPress={modelMenu}
          style={{ height: 28 * theme().scale, flexDirection: "row", alignItems: "center", minWidth: 0, gap: 4, paddingHorizontal: 5, borderRadius: 5 }}
          hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
          <Text numberOfLines={1} style={{ ...text(11), flexShrink: 1, minWidth: 0, fontWeight: "500" }}>{toolbar()?.modelLabel ?? ""}</Text>
          <Icon name="down" size={10 * theme().scale} color={theme().palette.muted} />
        </View>}
      </View>
      {spacious() && effortAvailable() && <View style={{ maxWidth: 96 * theme().scale, minWidth: 0, flexShrink: 1 }}>
        {pill("chat-effort", () => toolbar()?.effortLabel ?? "", effortMenu, theme().palette.muted)}
      </View>}
      {roomy() && hasToolbar() && <View style={{ maxWidth: 124 * theme().scale, minWidth: 0, flexShrink: 1 }}>
        {pill("chat-mode", () => toolbar()?.modeLabel ?? "", modeMenu, theme().palette.muted)}
      </View>}
      <View style={{ display: roomy() ? "flex" : "none", flexShrink: 0 }}><BillingControls control={props.billing} theme={theme} narrow={!spacious()} /></View>
      <View role="button" testID="chat-options" focusable={!!toolbar() || props.billing.available()} tooltip={optionsHint()} accessibilityLabel={optionsHint()}
        onPress={optionsMenu} style={{ width: 28 * theme().scale, height: 28 * theme().scale, flexShrink: 0, borderRadius: 5, alignItems: "center", justifyContent: "center" }}
        hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
        <Icon name="sliders" size={15 * theme().scale} color={toolbar()?.fast || toolbar()?.contributorMode === "on" ? theme().palette.cyan : theme().palette.muted} />
      </View>
      <View style={{ flexShrink: 0 }}><DictationControl theme={theme} enabled={dictationEnabled()} active={props.visible()} disabled={!accepting() || !!memo.preview()} value={draft} selection={selection} focused={focused}
        setValue={(value, start, end) => { draft.set(value); generation += 1; persist(); selection.set({ start, end }); input?.setSelection(start, end); }}
        focus={() => input?.focus()} onController={(controller) => dictation.set(controller)} send={submit} holdKey={holdKey()}
        sources={() => ({ names: [toolbar()?.providerLabel ?? "", ...attachments().map((entry) => entry.name)], paths: props.panel().cwd ? [props.panel().cwd!] : [], text: draft() })} /></View>
      {stopping()
        ? <View role="button" testID="chat-interrupt" focusable={true} tooltip={STOP_TIP} accessibilityLabel={STOP_TIP} onPress={interrupt}
          style={{ width: 30 * theme().scale, height: 30 * theme().scale, flexShrink: 0, borderRadius: 15 * theme().scale, alignItems: "center", justifyContent: "center", backgroundColor: theme().palette.gold }}
          hoverStyle={{ opacity: 0.85 }} focusStyle={{ opacity: 0.75 }}>
          <Icon name="stop" size={15 * theme().scale} color={theme().palette.bg} />
        </View>
        : <View role="button" testID="chat-send" focusable={!sendDisabled()} tooltip={props.panel().running ? STEER_TIP : SEND_TIP} accessibilityLabel={props.panel().running ? STEER_TIP : SEND_TIP}
          accessibilityState={{ disabled: sendDisabled() }} onPress={submit}
          style={{ width: 30 * theme().scale, height: 30 * theme().scale, flexShrink: 0, borderRadius: 15 * theme().scale, alignItems: "center", justifyContent: "center",
            backgroundColor: sendDisabled() ? theme().palette.panelActive : theme().palette.cyan }} hoverStyle={{ opacity: 0.85 }} focusStyle={{ opacity: 0.75 }}>
          <Icon name="up" size={17 * theme().scale} color={sendDisabled() ? theme().palette.muted : theme().palette.bg} />
        </View>}
    </View>
    </View>
  </View>;
}
