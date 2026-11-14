import { Scroll, Text, View, cleanup, computed, effect, keymap, signal, type ViewRef } from "nsh";
import { Button, Input, Radio } from "nsh/base";
import { Schedules } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";
import { focusView } from "../keyboard";
import { Segmented, StatusPill, buttonLook, cardStyle, focusRing, type ButtonKind, type Tone } from "../PanelKit";
import { PANEL } from "../panel_layout";
import {
  AutomationModel, EMPTY, EMPTY_TARGETS, FOOTNOTE, KEYS, PERMISSIONS, QUICK_TIMES,
  optionsLabel, projectScope, quickTime, selectScope, selectTarget, singleLine,
  status, targetsFor, visibleRows, type ChatOptions, type Editor, type ScheduleApi, type ScheduleData, type ScheduleRow,
} from "./model";

export interface AutomationsApi extends ScheduleApi {
  subscribe(listener: (event: { rev: number; data: ScheduleData }) => void): { close(): void };
  chatOptions(provider: string, projectId: string): Promise<ChatOptions>;
}

/** A retained panel. Native owns its process ticker; mounting/visibility only
 * opens or closes a changed-data subscription, never a dispatcher or UI poll. */
export function AutomationsPanel(props: {
  projectId: () => string;
  worktreeId?: () => string | null;
  visible?: () => boolean;
  active?: () => boolean;
  theme?: () => InterfaceTheme;
  openChat: (id: string) => void | Promise<void>;
  onPress?: () => void;
  now?: () => number;
  /** The fixture supplies an inert API; production uses the native boundary. */
  api?: AutomationsApi;
}) {
  const theme = props.theme ?? (() => DEFAULT_THEME), now = props.now ?? (() => Math.floor(Date.now() / 1000));
  const api = props.api ?? Schedules as AutomationsApi;
  const version = signal(0), focused = signal(false), composing = signal(false);
  // Notifications must not read version inside the binding/subscription effects:
  // doing so makes every editor update re-run those effects and stale options.
  let revision = 0;
  const changed = () => version.set(++revision);
  const model = new AutomationModel(api, changed, now);
  const state = () => { version(); return model; };
  const editor = () => state().editor;
  const editing = computed(() => editor() !== null);
  const pending = () => state().pending;
  const visible = () => props.visible?.() ?? true;
  const remote = () => props.projectId().startsWith("remote:");
  const palette = () => theme().palette;
  const text = (size = 11, color = palette().text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  let panel: ViewRef | undefined, title: ViewRef | undefined, subscription: { close(): void } | undefined;
  let optionsGeneration = 0, alive = true, focusTitle = false;
  effect(() => { optionsGeneration++; model.bind(props.projectId()); });
  effect(() => {
    subscription?.close(); subscription = undefined;
    if (!visible() || remote()) return;
    let revision = -1;
    const current = props.projectId();
    try {
      subscription = api.subscribe((event) => {
        if (!alive || current !== props.projectId() || event.rev <= revision) return;
        revision = event.rev;
        model.receive(event.data as ScheduleData);
      });
    } catch (reason) { model.error = String(reason); changed(); }
    return () => { subscription?.close(); subscription = undefined; };
  });
  cleanup(() => { alive = false; optionsGeneration++; subscription?.close(); });

  async function accountPreview(provider: string) {
    const token = ++optionsGeneration, project = props.projectId();
    try {
      const options = await api.chatOptions(provider, project);
      if (alive && token === optionsGeneration && project === props.projectId() && model.editor?.options.provider === provider && !model.editor.previous) {
        model.patch({ options: { ...model.editor.options, accountId: options.accountId } });
      }
    } catch (reason) {
      if (alive && token === optionsGeneration && project === props.projectId()) { model.error = String(reason); changed(); }
    }
  }
  function open(row?: ScheduleRow) {
    if (pending()) return;
    focusTitle = true;
    model.open(row);
    if (!row) void accountPreview("codex");
    title?.focus();
  }
  function cancel() { model.cancel(); focusView(panel); }
  // Escape steps back one level: a delete confirmation first, then the editor and its draft.
  function escape() { if (model.confirmDelete !== null) model.cancelDelete(); else cancel(); }
  async function save() {
    await model.save();
    // Validation and async rejection retain the invoking control's focus,
    // matching SchedulePanel::save. Kept inputs already own their native refs.
    if (alive && visible() && !model.editor) focusView(panel);
  }
  function options(patch: Partial<ChatOptions>) { if (model.editor) model.patch({ options: { ...model.editor.options, ...patch } }); }
  function changeProvider(provider: string) {
    if (!model.editor || model.editor.previous || pending()) return;
    options({ provider, accountId: null }); void accountPreview(provider);
  }
  function changeScope(scope: string) {
    if (!model.editor || pending()) return;
    model.patch(selectScope(model.editor, { scope, projectId: scope === "app" ? null : props.projectId(), worktreeId: scope === "workspace" ? props.worktreeId?.() ?? null : null }));
  }
  effect(() => {
    const active = props.active?.() ?? (focused() || editing() || state().confirmDelete !== null);
    if (!visible() || !active || composing() || pending()) return;
    const bindings = [keymap.bind("escape", escape, { when: "always" })];
    if (editor()) bindings.push(keymap.bind("mod+s", () => void save()));
    return () => bindings.forEach((dispose) => dispose());
  });

  /** A plain button, or a choice that is `selected`: raised on the quiet fill with an ink edge.
   * `kind` gives an action its look: the view's one primary, a red-labelled danger, a link. */
  function button(id: string, label: string, onPress: () => void, selected = false, disabled = pending(), kind: ButtonKind = "default") {
    const look = buttonLook(theme, kind, disabled);
    const chosen = selected && kind === "default";
    return <Button testID={id} accessibilityLabel={label} disabled={disabled} selected={selected} onPress={onPress}
      style={{ ...look.style, ...(chosen ? { backgroundColor: palette().panelActive, borderColor: palette().text } : {}) }}
      hoverStyle={chosen ? {} : look.hoverStyle} focusStyle={look.focusStyle} disabledStyle={look.disabledStyle}>
      <Text style={{ ...look.label, ...(chosen ? { fontWeight: "600" as const } : {}), numberOfLines: 1 }}>{label}</Text>
    </Button>;
  }
  /** A set of choices as one segmented control. */
  function segments(id: string, label: string, choices: { id: string; label: string; selected: boolean; disabled?: boolean; onPress: () => void }[]) {
    return <Segmented theme={theme} id={id} role="radio" accessibilityLabel={label} segments={choices} />;
  }
  function field(id: string, label: string, value: () => string, update: (value: string) => void, placeholder = label, isTitle = false) {
    return <View style={{ gap: 4, minWidth: 120, flex: 1 }}>
      <Text style={text(10, palette().muted)}>{label}</Text>
      <Input testID={`automation-${id}`} accessibilityLabel={label} placeholder={placeholder} readonly={pending()} value={value()}
        ref={(node) => { if (isTitle) { title = node; if (focusTitle) { focusTitle = false; node.focus(); } } }} onFocus={() => focused.set(true)} onCompositionChange={(event) => composing.set(event.composing)}
        onChange={({ value }) => { if (!pending()) update(value); }} style={{ ...text(), minHeight: space(28, theme().scale), backgroundColor: palette().panel, borderRadius: space(8, theme().scale), borderWidth: 1, borderColor: palette().divider, paddingHorizontal: space(8, theme().scale), paddingVertical: space(5, theme().scale) }}
        focusStyle={{ borderColor: palette().focus }} />
    </View>;
  }
  // Each set sits in a plain `role="radiogroup"` view, which is all NSH 0.3's RadioGroup was. The
  // 0.4+ RadioGroup makes a set one Tab stop whose arrow keys check the radio they reach.
  function choose(id: string, label: string, selected: boolean, onChange: () => void) {
    return <Radio testID={id} accessibilityLabel={label} checked={selected} disabled={pending()} onChange={onChange}>
      <Text style={text(10)}>{`${selected ? "●" : "○"} ${label}`}</Text>
    </Radio>;
  }
  function EditorView() {
    const e = () => editor()!;
    const fresh = computed(() => e().fresh), exact = computed(() => e().exact), customRepeat = computed(() => e().repeat === "minutes");
    const patch = (value: Partial<Editor>) => model.patch(value);
    const choices = () => targetsFor(state().data?.targets ?? [], e());
    return <View testID="automation-editor" style={{ ...cardStyle(theme), gap: space(10, theme().scale) }}>
      <Text style={{ ...text(13), fontWeight: "600" }}>{e().previous ? "Edit automation" : "New automation"}</Text>
      {segments("automation-destination", "Destination", [
        { id: "automation-destination-chat", label: "New project chat", selected: e().fresh, disabled: pending() || !!e().previous, onPress: () => patch({ fresh: true, scope: projectScope(props.projectId()), binding: null, pinned: false }) },
        { id: "automation-destination-shell", label: "Existing AI shell", selected: !e().fresh, disabled: pending() || !!e().previous, onPress: () => patch({ fresh: false, binding: null, pinned: false }) },
      ])}
      {field("title", "Title", () => e().title, (title) => patch({ title: singleLine(title) }), "e.g. Daily review", true)}
      {field("prompt", "Prompt · Single line", () => e().prompt, (prompt) => patch({ prompt: singleLine(prompt) }), "What should the agent do?")}
      {fresh() ? <>
        {segments("automation-provider", "Provider", ["codex", "claude", "muse"].map((provider) => ({ id: `automation-provider-${provider}`, label: provider[0].toUpperCase() + provider.slice(1),
          selected: e().options.provider === provider, disabled: pending() || !!e().previous, onPress: () => changeProvider(provider) })))}
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {field("model", "Model · Optional", () => e().options.model ?? "", (model) => options({ model }), "Default")}
          {field("effort", "Effort · Optional", () => e().options.effort ?? "", (effort) => options({ effort }), "Default")}
        </View>
        <Text style={text(10, palette().muted)}>Permission mode</Text>
        <View role="radiogroup" style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {PERMISSIONS.map(([value, label]) => choose(`automation-permission-${value}`, label, e().options.permission === value, () => options({ permission: value })))}
        </View>
        <View style={{ flexDirection: "row" }}>
          {button("automation-fast", `Fast mode: ${e().options.fast ? "On" : "Off"}`, () => options({ fast: !e().options.fast }), e().options.fast)}
        </View>
        <Text testID="automation-account" style={text(10, palette().muted)}>{e().options.provider === "codex" ? `Account: ${e().previous?.accountLabel ?? e().options.accountId ?? "System default"}` : `${e().options.provider === "claude" ? "Claude" : "Muse"} system login`}</Text>
        <Text style={text(10, palette().muted)}>Each run creates an ordinary chat at this project's root. The selected Codex account is pinned when saved.</Text>
      </> : <>
        {segments("automation-scope", "Scope", ["app", "project", "workspace"].map((scope) => ({ id: `automation-scope-${scope}`, label: scope[0].toUpperCase() + scope.slice(1),
          selected: e().scope.scope === scope, disabled: pending(), onPress: () => changeScope(scope) })))}
        {e().scope.scope === "workspace" && <View style={{ gap: 6 }}>
          <Text style={text(10, palette().muted)}>Workspace / worktree</Text>
          {(state().data?.worktrees ?? []).filter((tree) => tree.projectId === props.projectId()).map((tree) => button(`automation-worktree-${tree.id}`, `${tree.label} · ${tree.id}`, () => {
            if (model.editor) model.patch(selectScope(model.editor, { ...model.editor.scope, worktreeId: tree.id }));
          }, tree.id === e().scope.worktreeId))}
        </View>}
        {e().pinned && e().previous && <Text testID="automation-pinned" style={text(10, palette().muted)}>{`Pinned target · ${e().previous!.key.targetId}`}</Text>}
        <Text style={text(10, palette().muted)}>Existing target · Click to explicitly bind this session</Text>
        {!choices().length && <Text style={text(10, palette().muted)}>{e().scope.scope === "workspace" && !e().scope.worktreeId ? "Select a worktree in Worktrees before creating a workspace schedule" : EMPTY_TARGETS}</Text>}
        {choices().map((target) => button(`automation-target-${target.binding.kind}-${target.binding.id}`, target.label, () => {
          if (model.editor) model.patch(selectTarget(model.editor, target.binding));
        }, e().binding?.id === target.binding.id && e().binding?.kind === target.binding.kind))}
      </>}
      <Text style={text(10, palette().muted)}>First run</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {QUICK_TIMES.map(([seconds, label]) => button(`automation-first-${seconds}`, label, () => { if (model.editor) model.patch(quickTime(model.editor, seconds, now())); }, e().quick === seconds))}
        {button("automation-exact", "Exact time…", () => patch({ exact: !e().exact }), e().exact)}
      </View>
      {exact() && field("at", "Exact date / time · ISO with UTC offset", () => e().at, (at) => patch({ at, quick: null }), "e.g. 2030-01-31T09:00:00+01:00")}
      <Text testID="automation-local-time" style={text(10, palette().muted)}>{`Local · ${e().at.replace("T", " ")}`}</Text>
      <Text style={text(10, palette().muted)}>Repeat</Text>
      <View role="radiogroup" style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {([["once", "Once"], ["hourly", "Hourly"], ["daily", "24 hours"], ["weekly", "7 days"], ["minutes", "Minutes…"]] as const).map(([value, label]) => choose(`automation-repeat-${value}`, label, e().repeat === value, () => patch({ repeat: value })))}
      </View>
      {customRepeat() && field("minutes", "Every N minutes · Minimum 5", () => e().minutes, (minutes) => patch({ minutes }), "e.g. 90")}
      <Text style={text(10, palette().muted)}>Repeats use elapsed time from the first run (fixed UTC cadence, including DST).</Text>
      {!!state().error && <Text testID="automation-error" role="alert" style={text(11, palette().danger)}>{state().error}</Text>}
      <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 8 }}>
        {button("automation-save", pending() ? "Saving…" : e().previous ? "Save changes" : "Create automation", () => void save(), true, pending(), "primary")}
        {button("automation-cancel", "Cancel", cancel)}
        <Text style={{ ...text(10, palette().muted), paddingLeft: 4 }}>{KEYS}</Text>
      </View>
    </View>;
  }
  /** An automation as a card: its title and state, its prompt, when it runs next, then the rest
   * of what is known in one muted line (each fact on its own line in the tooltip). A failed,
   * uncertain or missed last run says so under it. */
  function renderRow(row: ScheduleRow) {
    const scope = row.key.scope.scope === "app" ? "App" : row.key.scope.scope === "workspace" ? "Workspace" : "Project";
    const tone: Tone = row.paused || row.reviewRequired ? "needs" : row.nextRun == null ? "neutral" : "done";
    const outcome = row.lastRun?.outcome;
    const troubled = outcome === "failed" || outcome === "uncertain" || outcome === "missed";
    const facts = [row.destinationLabel, row.lastRun ? `Last · ${row.lastRun.observedDisplay} · ${row.lastRun.outcome} · due ${row.lastRun.dueDisplay}` : "Last · —",
      !troubled && row.lastRun?.message ? row.lastRun.message : "", optionsLabel(row)].filter(Boolean);
    const line = (size = 10, color = palette().muted) => ({ ...text(size, color), minWidth: 0 });
    return <View testID={`automation-row-${row.key.id}`} style={{ ...cardStyle(theme, 14), gap: space(6, theme().scale) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
        <Text style={{ ...text(12), flex: 1, minWidth: 0, fontWeight: "600", numberOfLines: 1 }}>{`${scope} / ${row.title}`}</Text>
        <StatusPill theme={theme} id={`automation-status-${row.key.id}`} tone={tone} label={`${status(row)}${row.reviewRequired ? " · Review required" : ""}`} inline={true} />
      </View>
      <Text style={{ ...text(11), userSelect: "text", numberOfLines: 2 }}>{row.prompt}</Text>
      <Text style={{ ...line(10, palette().text), numberOfLines: 1 }}>{`Next · ${row.nextDisplay ?? "—"} · ${row.everyMinutes == null ? "Once" : `Every ${row.everyMinutes} min`}`}</Text>
      <View tooltip={<View style={{ maxWidth: space(320, theme().scale), padding: 7, gap: 3, borderRadius: 6, backgroundColor: palette().panelActive }}>{facts.map((fact) => <Text style={text(10)}>{fact}</Text>)}</View>}>
        <Text testID={`automation-facts-${row.key.id}`} style={{ ...line(), numberOfLines: 1 }}>{facts.join(" · ")}</Text>
      </View>
      {troubled && !!row.lastRun?.message && <Text style={{ ...line(10, outcome === "failed" ? palette().danger : palette().gold), numberOfLines: 2 }}>{row.lastRun.message}</Text>}
      <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: space(6, theme().scale), paddingTop: space(4, theme().scale) }}>
        {button(`automation-edit-${row.key.id}`, "Edit", () => open(row))}
        {button(`automation-pause-${row.key.id}`, row.paused ? "Resume" : "Pause", () => void model.pause(row))}
        {row.lastRun?.createdChatId && button(`automation-created-chat-${row.key.id}`, "Open created chat", () => { if (!pending()) void props.openChat(row.lastRun!.createdChatId!); }, false, pending(), "link")}
        <View style={{ flex: 1 }} />
        {state().confirmDelete === row.key.id && button(`automation-delete-cancel-${row.key.id}`, "Cancel", () => model.cancelDelete())}
        {button(`automation-delete-${row.key.id}`, state().confirmDelete === row.key.id ? "Confirm delete" : "Delete", () => void model.remove(row), false, pending(), "danger")}
      </View>
    </View>;
  }
  const rows = () => visibleRows(state().data?.rows ?? [], props.projectId());
  // The editor unfolds in place of the row it edits; a new automation (or a row that went away
  // mid-edit) opens it at the top. Both are computed so typing never remounts the inputs.
  const editingId = computed(() => {
    const id = editor()?.previous?.key.id;
    return id && rows().some((row) => row.key.id === id) ? id : null;
  });
  const inPlace = computed(() => editingId() !== null), atTop = computed(() => editing() && !inPlace());
  const split = () => { const all = rows(), at = all.findIndex((row) => row.key.id === editingId()); return at < 0 ? [all, []] : [all.slice(0, at), all.slice(at + 1)]; };
  const chats = () => (state().data?.chats ?? []).filter((chat) => chat.projectId === props.projectId());
  const schedulerError = () => state().data?.error || state().data?.runtimeError;
  const section = () => ({ ...text(10, palette().muted), fontWeight: "600" as const, paddingHorizontal: space(6, theme().scale), paddingTop: 4 });
  return <View ref={(node) => { panel = node; }} testID="automations-panel" onFocus={() => focused.set(true)} onBlur={() => focused.set(false)} onPress={props.onPress}
    style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
    {remote() ? <Text style={{ ...text(), padding: 16 }}>Automations works with this Mac's own projects. Select one of them to use it.</Text> : <>
      <View style={{ minHeight: space(PANEL.header, theme().scale), flexDirection: "row", alignItems: "center", paddingHorizontal: space(PANEL.inset, theme().scale), gap: 8 }}>
        <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
          <Text style={{ ...text(13), fontWeight: "600" }}>Automations</Text>
          <Text testID="automations-count" style={text(10, palette().muted)}>{!state().data ? "Reading automations…" : rows().length === 0 ? "No automations" : rows().length === 1 ? "1 automation" : `${rows().length} automations`}</Text>
        </View>
        <Button testID="automation-new" accessibilityLabel="New automation" disabled={pending()} onPress={() => open()}
          tooltip={<Text style={{ ...text(11), padding: 8, backgroundColor: palette().panelActive }}>New automation</Text>}
          style={{ width: space(PANEL.control, theme().scale), height: space(PANEL.control, theme().scale), borderRadius: space(8, theme().scale), alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={focusRing(palette(), 1)} disabledStyle={{ opacity: 0.4 }}>
          <Icon name="add" size={PANEL.icon * theme().scale} color={palette().muted} />
        </Button>
      </View>
      <Scroll testID="automations-scroll" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(PANEL.inset, theme().scale), paddingBottom: space(12, theme().scale) }}>
        <View style={{ width: "100%", gap: space(12, theme().scale) }}>
        {!!schedulerError() && <Text testID="automation-scheduler-error" role="alert" style={{ ...text(11, palette().danger), paddingHorizontal: space(6, theme().scale) }}>{`Scheduler error · ${schedulerError()}`}</Text>}
        {!editing() && !!state().error && <Text testID="automation-error" role="alert" style={{ ...text(11, palette().danger), paddingHorizontal: space(6, theme().scale) }}>{state().error}</Text>}
        {atTop() && <EditorView />}
        {rows().length > 0 && <Text style={section()}>Scheduled prompts</Text>}
        {state().data && !rows().length && !editing() && <View style={{ minHeight: space(120, theme().scale), padding: 24, gap: 8, alignItems: "center", justifyContent: "center" }}>
          <Icon name="calendar" size={22 * theme().scale} color={palette().muted} />
          <Text testID="automations-empty" style={{ ...text(11, palette().muted), textAlign: "center", maxWidth: space(280, theme().scale) }}>{EMPTY}</Text>
        </View>}
        {split()[0].map(renderRow)}
        {inPlace() && <EditorView />}
        {split()[1].map(renderRow)}
        {chats().length > 0 && <View style={{ gap: 6 }}>
          <View style={{ gap: 2 }}>
            <Text style={section()}>Project chats</Text>
            <Text style={{ ...text(10, palette().muted), paddingHorizontal: space(6, theme().scale) }}>Includes earlier automation results</Text>
          </View>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space(6, theme().scale), paddingHorizontal: space(6, theme().scale) }}>
            {chats().map((chat) => button(`automation-project-chat-${chat.id}`, `Open ${chat.title}`, () => { if (!pending()) void props.openChat(chat.id); }))}
          </View>
        </View>}
        {/* What Automations does and how it runs, once, under everything else. */}
        <View testID="automation-footnote-block" style={{ gap: space(3, theme().scale), paddingHorizontal: space(6, theme().scale), paddingTop: space(4, theme().scale), borderTopWidth: 1, borderColor: palette().divider }}>
          <Text style={{ ...text(9, palette().muted), paddingTop: space(8, theme().scale) }}>Schedule a new project chat or a prompt for an existing AI shell.</Text>
          <Text testID="automation-footnote" style={text(9, palette().muted)}>{pending() ? "Saving…" : FOOTNOTE}</Text>
        </View>
        </View>
      </Scroll>
    </>}
  </View>;
}
