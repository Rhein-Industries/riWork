import { Scroll, Text, View, cleanup, computed, effect, keymap, signal, type YNode } from "nsh";
import { Button, Input, Radio, RadioGroup } from "nsh/base";
import { Schedules } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import {
  AutomationModel, EMPTY, EMPTY_TARGETS, FOOTNOTE, PERMISSIONS, QUICK_TIMES,
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
  let panel: YNode | undefined, title: YNode | undefined, subscription: { close(): void } | undefined;
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
  function cancel() { model.cancel(); panel?.focus(); }
  async function save() {
    await model.save();
    if (alive && visible()) { if (model.editor) title?.focus(); else panel?.focus(); }
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
    const bindings = [keymap.bind("escape", cancel, { when: "always" })];
    if (editor()) bindings.push(keymap.bind("mod+s", () => void save()));
    return () => bindings.forEach((dispose) => dispose());
  });

  function button(id: string, label: string, onPress: () => void, selected = false, disabled = pending()) {
    return <Button testID={id} accessibilityLabel={label} disabled={disabled} selected={selected} onPress={onPress}
      style={{ paddingHorizontal: 9, paddingVertical: 5, borderWidth: 1, borderColor: selected ? palette().cyan : palette().divider, borderRadius: 12, backgroundColor: selected ? palette().panelActive : palette().panel }}>
      <Text style={text(10, selected ? palette().cyan : disabled ? palette().muted : palette().text)}>{label}</Text>
    </Button>;
  }
  function field(id: string, label: string, value: () => string, update: (value: string) => void, isTitle = false) {
    return <View style={{ gap: 4, minWidth: 120, flex: 1 }}>
      <Text style={text(10, palette().muted)}>{label}</Text>
      <Input testID={`automation-${id}`} accessibilityLabel={label} placeholder={label} readonly={pending()} value={value()}
        ref={(node) => { if (isTitle) { title = node; if (focusTitle) { focusTitle = false; node.focus(); } } }} onFocus={() => focused.set(true)} onCompositionChange={(event) => composing.set(event.composing)}
        onChange={(value) => { if (!pending()) update(value); }} style={{ ...text(), borderWidth: 1, borderColor: palette().divider, borderRadius: 4, paddingHorizontal: 7, paddingVertical: 5 }} />
    </View>;
  }
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
    return <View testID="automation-editor" style={{ gap: 10, padding: 12, borderWidth: 1, borderColor: palette().divider, borderRadius: 6, backgroundColor: palette().panel }}>
      <Text style={text(12)}>{e().previous ? "Edit automation" : "New automation"}</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {button("automation-destination-chat", "New project chat", () => patch({ fresh: true, scope: projectScope(props.projectId()), binding: null, pinned: false }), e().fresh, pending() || !!e().previous)}
        {button("automation-destination-shell", "Existing AI shell", () => patch({ fresh: false, binding: null, pinned: false }), !e().fresh, pending() || !!e().previous)}
      </View>
      {field("title", "Title", () => e().title, (title) => patch({ title: singleLine(title) }), true)}
      {field("prompt", "Prompt · Single line", () => e().prompt, (prompt) => patch({ prompt: singleLine(prompt) }))}
      {fresh() ? <>
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {["codex", "claude", "muse"].map((provider) => button(`automation-provider-${provider}`, provider[0].toUpperCase() + provider.slice(1), () => changeProvider(provider), e().options.provider === provider, pending() || !!e().previous))}
        </View>
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {field("model", "Model · Optional", () => e().options.model ?? "", (model) => options({ model }))}
          {field("effort", "Effort · Optional", () => e().options.effort ?? "", (effort) => options({ effort }))}
        </View>
        <Text style={text(10, palette().muted)}>Permission mode</Text>
        <RadioGroup style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {PERMISSIONS.map(([value, label]) => choose(`automation-permission-${value}`, label, e().options.permission === value, () => options({ permission: value })))}
        </RadioGroup>
        {button("automation-fast", `Fast mode: ${e().options.fast ? "On" : "Off"}`, () => options({ fast: !e().options.fast }), e().options.fast)}
        <Text testID="automation-account" style={text(10, palette().muted)}>{e().options.provider === "codex" ? `Account: ${e().previous?.accountLabel ?? e().options.accountId ?? "System default"}` : `${e().options.provider === "claude" ? "Claude" : "Muse"} system login`}</Text>
        <Text style={text(10, palette().muted)}>Each run creates an ordinary chat at this project's root. The selected Codex account is pinned when saved.</Text>
      </> : <>
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
          {["app", "project", "workspace"].map((scope) => button(`automation-scope-${scope}`, scope[0].toUpperCase() + scope.slice(1), () => changeScope(scope), e().scope.scope === scope))}
        </View>
        {e().scope.scope === "workspace" && <View style={{ gap: 6 }}>
          <Text style={text(10, palette().muted)}>Workspace / worktree</Text>
          {(state().data?.worktrees ?? []).filter((tree) => tree.projectId === props.projectId()).map((tree) => button(`automation-worktree-${tree.id}`, `${tree.label} · ${tree.id}`, () => {
            if (model.editor) model.patch(selectScope(model.editor, { ...model.editor.scope, worktreeId: tree.id }));
          }, tree.id === e().scope.worktreeId))}
        </View>}
        {e().pinned && e().previous && <Text testID="automation-pinned" style={text(10, palette().muted)}>{`PINNED TARGET / ${e().previous!.key.targetId}`}</Text>}
        <Text style={text(10, palette().muted)}>Existing target · Click to explicitly bind this session</Text>
        {!choices().length && <Text style={text(10, palette().gold)}>{e().scope.scope === "workspace" && !e().scope.worktreeId ? "Select a worktree in WORKTREES before creating a workspace schedule" : EMPTY_TARGETS}</Text>}
        {choices().map((target) => button(`automation-target-${target.binding.kind}-${target.binding.id}`, target.label, () => {
          if (model.editor) model.patch(selectTarget(model.editor, target.binding));
        }, e().binding?.id === target.binding.id && e().binding?.kind === target.binding.kind))}
      </>}
      <Text style={text(10, palette().muted)}>First run</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {QUICK_TIMES.map(([seconds, label]) => button(`automation-first-${seconds}`, label, () => { if (model.editor) model.patch(quickTime(model.editor, seconds, now())); }, e().quick === seconds))}
        {button("automation-exact", "Exact time…", () => patch({ exact: !e().exact }), e().exact)}
      </View>
      {exact() && field("at", "Exact date / time · ISO with UTC offset", () => e().at, (at) => patch({ at, quick: null }))}
      <Text testID="automation-local-time" style={text(10, palette().muted)}>{`LOCAL ${e().at.replace("T", " ")}`}</Text>
      <Text style={text(10, palette().muted)}>Repeat</Text>
      <RadioGroup style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {([["once", "Once"], ["hourly", "Hourly"], ["daily", "24 hours"], ["weekly", "7 days"], ["minutes", "Minutes…"]] as const).map(([value, label]) => choose(`automation-repeat-${value}`, label, e().repeat === value, () => patch({ repeat: value })))}
      </RadioGroup>
      {customRepeat() && field("minutes", "Every N minutes · Minimum 5", () => e().minutes, (minutes) => patch({ minutes }))}
      <Text style={text(10, palette().muted)}>Repeats use elapsed time from the first run (fixed UTC cadence, including DST).</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {button("automation-save", e().previous ? "Save changes" : "Create automation", () => void save())}
        {button("automation-cancel", "Cancel", cancel)}
      </View>
    </View>;
  }
  function renderRow(row: ScheduleRow) {
    return <View testID={`automation-row-${row.key.id}`} style={{ gap: 5, padding: 12, borderWidth: 1, borderColor: palette().divider, borderRadius: 6 }}>
      <Text style={text(12)}>{row.title}</Text>
      <Text style={text(10, row.reviewRequired ? palette().gold : palette().muted)}>{`${status(row)} · ${row.everyMinutes == null ? "Once" : `Every ${row.everyMinutes} min`}${row.reviewRequired ? " · Review required" : ""}`}</Text>
      <Text style={text(10, palette().muted)}>{`NEXT ${row.nextDisplay ?? "—"}`}</Text>
      <Text style={text(10, palette().muted)}>{row.lastRun ? `LAST ${row.lastRun.observedDisplay} · ${row.lastRun.outcome} · due ${row.lastRun.dueDisplay}` : "LAST —"}</Text>
      {row.lastRun?.message && <Text style={text(10, row.lastRun.outcome === "failed" || row.lastRun.outcome === "uncertain" ? palette().gold : palette().muted)}>{row.lastRun.message}</Text>}
      <Text style={text(10, palette().muted)}>{row.destinationLabel}</Text>
      <Text style={text(10, palette().muted)}>{optionsLabel(row)}</Text>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
        {button(`automation-edit-${row.key.id}`, "Edit", () => open(row))}
        {button(`automation-pause-${row.key.id}`, row.paused ? "Resume" : "Pause", () => void model.pause(row))}
        {button(`automation-delete-${row.key.id}`, state().confirmDelete === row.key.id ? "Confirm delete" : "Delete", () => void model.remove(row))}
        {row.lastRun?.createdChatId && button(`automation-created-chat-${row.key.id}`, "Open created chat", () => { if (!pending()) void props.openChat(row.lastRun!.createdChatId!); })}
      </View>
    </View>;
  }
  const rows = () => visibleRows(state().data?.rows ?? [], props.projectId());
  return <View ref={(node) => { panel = node; }} testID="automations-panel" focusable={true} onFocus={() => focused.set(true)} onBlur={() => focused.set(false)} onPress={props.onPress}
    style={{ flex: 1, minHeight: 0, backgroundColor: palette().bg }}>
    {remote() ? <Text style={{ ...text(), padding: 16 }}>Automations works with this Mac's own projects. Select one of them to use it.</Text> : <>
      <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", padding: 12, gap: 8 }}>
        <Text style={text(13)}>Automations</Text>
        <Text testID="automations-count" style={text(10, palette().muted)}>{!state().data ? "Reading automations…" : rows().length === 0 ? "No automations" : rows().length === 1 ? "1 automation" : `${rows().length} automations`}</Text>
        {button("automation-new", "+ New automation", () => open())}
      </View>
      <Scroll testID="automations-scroll" style={{ flex: 1, minHeight: 0 }} contentStyle={{ gap: 10, paddingHorizontal: 12, paddingBottom: 12 }}>
        {(state().error || state().data?.error || state().data?.runtimeError) && <Text testID="automation-error" role="alert" style={text(11, palette().gold)}>{`SCHEDULER ERROR / ${state().error || state().data?.error || state().data?.runtimeError}`}</Text>}
        {editing() && <EditorView />}
        <Text style={text(11, palette().muted)}>Scheduled prompts</Text>
        {state().data && !rows().length && <Text testID="automations-empty" style={text(11, palette().muted)}>{EMPTY}</Text>}
        {rows().map(renderRow)}
        {(state().data?.chats ?? []).filter((chat) => chat.projectId === props.projectId()).length > 0 && <View style={{ gap: 6 }}>
          <Text style={text(11, palette().muted)}>Project chats · Includes earlier automation results</Text>
          {(state().data?.chats ?? []).filter((chat) => chat.projectId === props.projectId()).map((chat) => button(`automation-project-chat-${chat.id}`, `Open ${chat.title} · ${chat.id}`, () => { if (!pending()) void props.openChat(chat.id); }))}
        </View>}
        <Text testID="automation-footnote" style={text(10, palette().muted)}>{pending() ? "Saving…" : FOOTNOTE}</Text>
      </Scroll>
    </>}
  </View>;
}
