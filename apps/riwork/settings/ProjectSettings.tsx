import { Scroll, Text, View, cleanup, effect, keymap, signal, untracked, type YNode } from "nsh";
import { Button, Input } from "nsh/base";
import { Accounts, ProjectSettings as NativeProjectSettings, Settings, Store, type AccountsDto, type ProjectSettingsDto } from "riwork/native";
import { MemoEditor } from "../Files";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { ChoiceRow, Section, StepperRow, buttonStyle, textStyle } from "./SettingsControls";
import { ProjectDraft } from "./project_model";

type ProjectSettingsProps = {
  projectId: () => string; theme?: () => InterfaceTheme; visible?: () => boolean;
  onControl?: (control: { revealMemo: () => void } | null) => void;
};
const providerChoices = ["codex", "claude", "muse"].map((id) => ({ id: `project-orchestrator-provider-${id}`, label: id[0].toUpperCase() + id.slice(1) }));

/** Metadata is an explicit form; account and agent choices persist independently. */
export function ProjectSettingsPanel(props: ProjectSettingsProps) {
  const theme = props.theme ?? (() => DEFAULT_THEME), shown = () => props.visible?.() ?? true;
  const snapshot = signal<ProjectSettingsDto | null>(null), accounts = signal<AccountsDto | null>(null);
  // Keep the form's native inputs mounted while their values and status change.
  const ready = signal(false);
  // `pending`: any save in flight (controls wait); `saving`: the name and folder form's own save.
  const error = signal(""), pending = signal(false), saving = signal(false), accountPending = signal(false), typedModel = signal("");
  const folderName = signal(""), revision = signal(0);
  const drafts = new Map<string, ProjectDraft>();
  let alive = true, generation = 0, current = "", accountRefreshStarted = false;
  let nameInput: YNode | undefined, scroll: YNode | undefined;
  let memoControl: { save: () => void; focus: () => void } | null = null;
  let revealPending = false;
  let revealTimer: ReturnType<typeof setTimeout> | undefined;
  const redraw = () => { if (alive) revision.set(revision() + 1); };
  const draft = () => { revision(); return drafts.get(props.projectId()); };
  function focusMemo() {
    if (!memoControl) return;
    revealPending = false; clearTimeout(revealTimer);
    revealTimer = setTimeout(() => { if (alive) { scroll?.scrollToEnd({ animated: false }); memoControl?.focus(); } }, 0);
  }
  function revealMemo() { revealPending = true; focusMemo(); }
  props.onControl?.({ revealMemo });
  async function refresh() {
    if (!alive || !shown() || !props.projectId()) return;
    const id = props.projectId(), token = ++generation;
    try {
      const next = await NativeProjectSettings.get(id);
      if (!alive || token !== generation || id !== props.projectId()) return;
      if (!drafts.has(id)) drafts.set(id, new ProjectDraft(next.project));
      else drafts.get(id)!.refresh(next.project, next.folders);
      snapshot.set(next); redraw(); ready.set(true);
    } catch (reason) { if (alive && token === generation) error.set(String(reason)); }
  }
  async function refreshAccounts() {
    if (!alive || accountPending()) return;
    accountPending.set(true);
    try { const next = await Accounts.refresh(); if (alive) { accounts.set(next); await refresh(); } }
    catch (reason) { if (alive) error.set(String(reason)); }
    finally { if (alive) accountPending.set(false); }
  }
  // Track the project and visibility only: Accounts events refresh through their own subscription.
  effect(() => {
    const id = props.projectId(), visible = shown();
    untracked(() => {
      if (id !== current) { current = id; ready.set(false); snapshot.set(null); folderName.set(""); typedModel.set(""); error.set(""); }
      if (visible) {
        void refresh();
        if (accounts() && !accounts()!.listed && !accountRefreshStarted) { accountRefreshStarted = true; void refreshAccounts(); }
      }
    });
  });
  const subscriptions = [Store.subscribe(() => { void refresh(); }), Settings.subscribe(() => { void refresh(); }), Accounts.subscribe((event) => {
    if (!alive) return;
    accounts.set(event.data); void refresh();
    if (shown() && !event.data.listed && !accountRefreshStarted) { accountRefreshStarted = true; void refreshAccounts(); }
  })];
  cleanup(() => { alive = false; generation++; clearTimeout(revealTimer); props.onControl?.(null); for (const subscription of subscriptions) subscription.close(); });
  async function run(action: () => Promise<unknown>, message: string) {
    if (!alive || pending()) return;
    const id = props.projectId(); pending.set(true); error.set("");
    try {
      await action();
      if (alive && id === props.projectId()) { drafts.get(id)?.immediateSaved(message); redraw(); await refresh(); }
    } catch (reason) { if (alive && id === props.projectId()) error.set(String(reason)); }
    finally { if (alive) pending.set(false); }
  }
  async function save() {
    if (!alive) return;
    const form = draft(); if (!form || !canSave()) return;
    const request = form.request();
    saving.set(true);
    try {
      await run(async () => {
        const result = await Store.updateProjectMetadata(request.id, request.name, request.folderId);
        form.saved(request, result);
      }, "Settings saved");
    } finally { if (alive) saving.set(false); }
  }
  /** Only edited metadata with a name saves; immediate choices never need the button. */
  function canSave() {
    const form = draft();
    return !!form && form.pending && !!form.name.trim() && !pending() && snapshot()?.project.id === props.projectId();
  }
  /** Shown beside Save: the hint while edits wait, else the last confirmation, else nothing. */
  function saveStatus() {
    const form = draft();
    if (!form || (!form.pending && !form.notice)) return "";
    return form.pending && !form.name.trim() ? "Enter a name to save" : form.message();
  }
  function saveAll() { memoControl?.save(); void save(); }
  effect(() => { if (shown()) return keymap.bind("mod+s", saveAll); });
  async function addFolder() {
    const form = draft(); if (!form || !folderName().trim()) return;
    const id = props.projectId(), text = folderName(), parent = form.folderId;
    await run(async () => {
      const created = await Store.createFolder(text, parent);
      if (alive && id === props.projectId()) { form.chooseFolder(created.id); if (folderName() === text) folderName.set(""); nameInput?.focus(); }
    }, "");
  }
  const paths = () => (snapshot()?.folders ?? []).map((folder) => ({ id: folder.id, label: folder.path }));
  const accountId = () => {
    const choice = snapshot()?.project.codexAccount;
    return choice?.source === "saved" ? `project-codex-saved-${choice.accountId}` : choice?.source === "system_default" ? "project-codex-system" : "project-codex-inherit";
  };
  function chooseAccount(id: string) {
    const choice = snapshot()?.accounts.find((account) => accountKey(account) === id);
    if (!choice?.available) return;
    void run(() => Store.setProjectCodexAccount(props.projectId(), { source: choice.source, accountId: choice.accountId }), "Codex account saved");
  }
  function accountKey(choice: { source: string; accountId?: string | null }) {
    return choice.source === "saved" ? `project-codex-saved-${choice.accountId}` : choice.source === "system_default" ? "project-codex-system" : "project-codex-inherit";
  }
  const orchestrator = () => snapshot()!.orchestrator;
  const mode = () => snapshot()?.orchestrator.selectedMode ?? "";
  function chooseOrchestrator(choice: Record<string, unknown>) {
    return run(() => Store.setProjectOrchestrator(props.projectId(), choice), "Orchestrator choice saved");
  }
  function chooseMode(id: string) {
    const picked = id.replace("project-orchestrator-", ""), settings = snapshot()?.settings;
    if (picked === "chat") {
      if (mode() === "chat" || !settings) return;
      void chooseOrchestrator({ mode: "chat", provider: settings.orchestratorChatProvider, model: settings.orchestratorChatModel });
    } else void chooseOrchestrator({ mode: picked });
  }
  const modelChoices = () => snapshot()?.orchestrator.models ?? [];
  const selectedModel = () => modelChoices().find((choice) => (choice.model ?? null) === (orchestrator().model ?? null))?.id ?? "";
  async function applyTypedModel() {
    if (!alive || pending() || !typedModel().trim()) return;
    const typed = typedModel(), id = props.projectId();
    await chooseOrchestrator({ mode: "chat", provider: orchestrator().provider, model: typed.trim() });
    if (alive && id === props.projectId() && !error() && typedModel() === typed) typedModel.set("");
  }
  const selectedSpread = () => snapshot()?.delegation.selectedSpreadWork === true ? "on" : snapshot()?.delegation.selectedSpreadWork === false ? "off" : "inherit";
  function delegation(patch: Parameters<typeof Store.setProjectDelegationPolicy>[1]) {
    void run(() => Store.setProjectDelegationPolicy(props.projectId(), patch), "Delegation policy saved");
  }
  function routeId(work: "design" | "workhorse") {
    const chosen = snapshot()?.delegation[work === "design" ? "selectedDesign" : "selectedWorkhorse"], choices = snapshot()?.delegation.effective[work === "design" ? "designChoices" : "workhorseChoices"] ?? [];
    if (!chosen) return `project-delegation-${work}-inherit`;
    return `project-delegation-${work}-${choices.find((choice) => choice.route.provider === chosen.provider && choice.route.model === chosen.model)?.id ?? "unreadable"}`;
  }
  const fieldStyle = () => ({ minHeight: 30, padding: 8, borderRadius: 4, backgroundColor: theme().palette.bg, ...textStyle(theme, 12) });
  const note = (text: string | null | undefined, id: string) => text ? <Text testID={id} role="alert" style={{ ...textStyle(theme, 10), color: theme().palette.gold }}>{text}</Text> : null;
  const compact = () => ({ ...buttonStyle(theme), alignSelf: "flex-start" as const });
  // Off / On chips, as Spread work: a per-project switch that saves when chosen. `value` is read
  // lazily so a refresh updates the row instead of rebuilding it (and dropping its focus).
  const onOff = (id: string, label: string, description: string | undefined, value: () => boolean, onChange: (enabled: boolean) => void) =>
    <ChoiceRow id={id} label={label} description={description} value={`${id}-${value() ? "on" : "off"}`} choices={[{ id: `${id}-off`, label: "Off" }, { id: `${id}-on`, label: "On" }]}
      disabled={pending()} onChange={(choice) => { const enabled = choice.endsWith("-on"); if (enabled !== value()) onChange(enabled); }} theme={theme} />;
  // The form's one action lives in the fixed header: always in view, filled only when it can save.
  // Props read `canSave()` lazily, so the button updates in place and keeps its focus.
  const saveButton = () => <Button testID="project-settings-save" accessibilityLabel="Save name and folder" disabled={!canSave()} onPress={() => void save()}
    style={{ ...buttonStyle(theme), backgroundColor: canSave() ? theme().palette.cyan : theme().palette.panelActive, opacity: canSave() || saving() ? 1 : 0.6 }}>
    <Text style={{ ...textStyle(theme), color: canSave() ? theme().palette.bg : theme().palette.muted }}>{saving() ? "Saving…" : "Save"}</Text>
  </Button>;
  return <View testID="project-settings-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <View style={{ borderBottomWidth: 1, borderColor: theme().palette.divider }}>
      <View style={{ minHeight: space(46, theme().scale), paddingHorizontal: space(12, theme().scale), paddingVertical: space(6, theme().scale), flexDirection: "row", alignItems: "center", gap: space(10, theme().scale) }}>
        <View style={{ flex: 1, minWidth: 0, gap: space(1, theme().scale) }}>
          <Text role="header" numberOfLines={1} style={{ ...textStyle(theme, 13), fontWeight: "600" }}>Project settings</Text>
          <View style={{ flexDirection: "row", alignItems: "center", gap: space(6, theme().scale), minWidth: 0 }}>
            <Text numberOfLines={1} style={{ ...textStyle(theme, 10, true), flexShrink: 1 }}>{snapshot()?.project.name ?? ""}</Text>
            {ready() && <Text testID="project-settings-badge" style={{ ...textStyle(theme, 9, true), paddingHorizontal: space(5, theme().scale), paddingVertical: 1, borderRadius: 4, backgroundColor: theme().palette.panelActive, ...(draft()!.pending ? { color: theme().palette.gold } : {}) }}>{draft()!.pending ? "Unsaved" : "Local project"}</Text>}
          </View>
        </View>
        {ready() && saveStatus() ? <Text testID="project-settings-save-status" numberOfLines={1} style={{ ...textStyle(theme, 10, true), flexShrink: 1 }}>{saveStatus()}</Text> : null}
        {ready() && saveButton()}
      </View>
      {ready() && (error() || snapshot()!.settings.error) ? <View style={{ paddingHorizontal: space(12, theme().scale), paddingBottom: space(8, theme().scale), gap: space(2, theme().scale) }}>
        {note(error(), "project-settings-error")}
        {note(snapshot()!.settings.error, "project-settings-preferences-error")}
      </View> : null}
    </View>
    <Scroll ref={(node) => { scroll = node; }} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: space(12, theme().scale) }}>
    {!ready() ? <Text style={{ ...textStyle(theme), padding: space(6, theme().scale), paddingTop: space(10, theme().scale) }}>{error() || (props.projectId() ? "Reading project settings…" : "Select a local project to edit its settings.")}</Text> : <View style={{ gap: 0, minWidth: 0, width: "100%", maxWidth: 820, alignSelf: "center" }}>
      <Section number="01" title="Name" theme={theme}><Input testID="project-settings-name" ref={(node) => { nameInput = node; }} accessibilityLabel="Project name" value={draft()!.name} onChange={(name) => { draft()!.editName(name); error.set(""); redraw(); }} onSubmit={() => void save()} style={fieldStyle()} /></Section>
      <Section number="02" title="Virtual folder" description="Group projects in the browser. Files stay in their current locations." theme={theme}>
        <ChoiceRow id="project-settings-folders" label="Folder" value={draft()!.folderId ?? "project-folder-unfiled"} disabled={pending()} theme={theme}
          choices={[{ id: "project-folder-unfiled", label: "Unfiled" }, ...paths()]}
          onChange={(id) => { draft()!.chooseFolder(id === "project-folder-unfiled" ? null : id); error.set(""); redraw(); }} />
        {draft()!.folderId && <Text style={textStyle(theme, 10, true)}>{`New subfolders go under ${paths().find((folder) => folder.id === draft()!.folderId)?.label ?? "the selected folder"}`}</Text>}
        <View style={{ flexDirection: "row", alignItems: "center", gap: space(8, theme().scale) }}>
          <Input testID="project-settings-folder-name" accessibilityLabel={draft()!.folderId ? "New subfolder" : "New virtual folder"} placeholder={draft()!.folderId ? "New subfolder" : "New virtual folder"} value={folderName()} onChange={(value) => folderName.set(value)} onSubmit={() => void addFolder()} style={{ ...fieldStyle(), flex: 1, minWidth: 0 }} />
          <Button testID="project-settings-create-folder" disabled={pending() || !folderName().trim()} onPress={() => void addFolder()} style={{ ...buttonStyle(theme), opacity: pending() || !folderName().trim() ? 0.6 : 1 }}><Text style={textStyle(theme)}>{draft()!.folderId ? "Add subfolder" : "Add folder"}</Text></Button>
        </View>
      </Section>
      <Section number="03" title="Codex account" description="New Codex sessions use this choice. Running sessions keep their account. Selection saves immediately." theme={theme}>
        <ChoiceRow id="project-codex-accounts" label="Account" value={accountId()} choices={snapshot()!.accounts.map((choice) => ({ id: accountKey(choice), label: choice.label, disabled: !choice.available, description: choice.reason ?? undefined }))} disabled={pending()} onChange={chooseAccount} theme={theme} />
        {snapshot()!.selectedAccountUnavailable && note("Selected account unavailable. Choose another before starting Codex.", "project-codex-unavailable")}
        {note(accounts()?.error, "project-codex-error")}
        <Button testID="project-codex-refresh" disabled={accountPending() || accounts()?.refreshing} onPress={() => void refreshAccounts()} style={compact()}><Text style={textStyle(theme)}>{accountPending() || accounts()?.refreshing ? "Checking accounts…" : "Refresh accounts"}</Text></Button>
      </Section>
      <Section number="04" title="Orchestrator" description="Applies when this project's orchestrator is next created. Selection saves immediately." theme={theme}>
        <ChoiceRow id="project-orchestrator-modes" label="Orchestrator runs as" value={`project-orchestrator-${mode()}`} choices={[["inherit", "Inherit app default"], ["automatic", "Automatic"], ["terminal", "Terminal"], ["chat", "Chat"]].map(([id, label]) => ({ id: `project-orchestrator-${id}`, label }))} disabled={pending()} onChange={chooseMode} theme={theme} />
        {note(snapshot()!.orchestrator.unreadableNote, "project-orchestrator-unreadable")}
        <Text style={textStyle(theme, 10, true)}>{`New orchestrators use ${snapshot()!.orchestrator.effective.description}.`}</Text>
        {mode() === "chat" && <View style={{ gap: 6 }}>
          <ChoiceRow id="project-orchestrator-providers" label="Chat provider" value={`project-orchestrator-provider-${orchestrator().provider}`} choices={providerChoices} disabled={pending()} onChange={(id) => { const provider = id.replace("project-orchestrator-provider-", ""); void chooseOrchestrator({ mode: "chat", provider, model: provider === orchestrator().provider ? orchestrator().model ?? null : null }); }} theme={theme} />
          <ChoiceRow id="project-orchestrator-models" label="Chat model" value={`project-orchestrator-model-${selectedModel()}`} choices={modelChoices().map((choice) => ({ id: `project-orchestrator-model-${choice.id}`, label: choice.label }))} disabled={pending()} onChange={(id) => { const choice = modelChoices().find((choice) => `project-orchestrator-model-${choice.id}` === id); if (choice) void chooseOrchestrator({ mode: "chat", provider: orchestrator().provider, model: choice.model }); }} theme={theme} />
          <Input testID="project-orchestrator-model-input" accessibilityLabel="Other model ID" placeholder="Other model ID" value={typedModel()} onChange={(value) => typedModel.set(value)} onSubmit={() => void applyTypedModel()} style={fieldStyle()} />
        </View>}
      </Section>
      <Section number="05" title="Delegation" theme={theme}>
        {note(snapshot()!.delegation.effective.unreadableNote, "project-delegation-unreadable")}
        <ChoiceRow id="project-delegation-spread" label="Spread work" value={`project-delegation-spread-${selectedSpread()}`} choices={[["inherit", "Inherit"], ["on", "On"], ["off", "Off"]].map(([id, label]) => ({ id: `project-delegation-spread-${id}`, label }))} disabled={pending()} onChange={(id) => delegation({ spreadWork: id.endsWith("inherit") ? null : id.endsWith("on") })} theme={theme} />
        <StepperRow id="project-delegation-parallel" decreaseId="project-delegation-parallel-fewer" increaseId="project-delegation-parallel-more" label="Parallel worker shells" description="A soft target; the orchestrator decides how many worker shells to start." value={snapshot()!.delegation.effective.parallelShells} minimum={1} maximum={10} disabled={pending()} onChange={(parallelShells) => delegation({ parallelShells })} theme={theme}>
          <Button testID="project-delegation-parallel-inherit" selected={snapshot()!.delegation.selectedParallelShells == null} disabled={pending()} onPress={() => delegation({ parallelShells: null })} style={buttonStyle(theme)}><Text style={textStyle(theme)}>Inherit</Text></Button>
        </StepperRow>
        {(["design", "workhorse"] as const).map((work) => <ChoiceRow id={`project-delegation-${work}`} label={work === "design" ? "Design work" : "Workhorse work"} value={routeId(work)} choices={[{ id: `project-delegation-${work}-inherit`, label: "Inherit" }, ...snapshot()!.delegation.effective[work === "design" ? "designChoices" : "workhorseChoices"].map((choice) => ({ id: `project-delegation-${work}-${choice.id}`, label: choice.label }))]} disabled={pending()} onChange={(id) => {
          const choice = snapshot()!.delegation.effective[work === "design" ? "designChoices" : "workhorseChoices"].find((choice) => `project-delegation-${work}-${choice.id}` === id);
          delegation({ [work]: choice?.route ?? null });
        }} theme={theme} />)}
      </Section>
      <Section number="06" title="Notifications" description="When an agent in this project finishes. Changes save immediately." theme={theme}>
        {onOff("project-notify-agent-done", "Agent completion notifications", undefined, () => snapshot()?.project.notifyOnAgentDone === true,
          (enabled) => void run(() => Store.setProjectNotifications(props.projectId(), enabled), "Notifications saved"))}
        {onOff("project-wake-orchestrators", "Wake orchestrators when a worker finishes", "A worker shell's finished turn checks in with this project's orchestrator and the global one.", () => snapshot()?.project.wakeOrchestratorsOnAgentDone === true,
          (enabled) => void run(() => Store.setProjectOrchestratorWakes(props.projectId(), enabled), "Orchestrator wakes saved"))}
      </Section>
      <Section number="07" title="Locations" theme={theme}>
        <Text selectable={true} style={{ ...textStyle(theme), fontFamily: theme().monoFamily }}>{snapshot()!.project.root}</Text>
        {snapshot()!.project.repositoryRoots.length ? <View style={{ gap: space(4, theme().scale) }}>
          <Text style={textStyle(theme)}>{`Repositories · ${snapshot()!.project.repositoryRoots.length}`}</Text>
          {snapshot()!.project.repositoryRoots.map((root) => <Text selectable={true} style={{ ...textStyle(theme, 10), fontFamily: theme().monoFamily }}>{root}</Text>)}
        </View> : <Text style={textStyle(theme, 10, true)}>No Git repositories detected</Text>}
        <Text selectable={true} style={textStyle(theme, 10, true)}>{`Project ID ${snapshot()!.project.id}`}</Text>
      </Section>
      <Section number="08" title="Memo" description="Your own Markdown notes for this project, saved as you type and shared with RiWork on your phone. Type /memo in one of its chats to insert them." theme={theme}>
        <MemoEditor projectId={props.projectId} theme={theme} visible={shown} bindSaveKey={false} onControl={(control) => { memoControl = control; if (control && revealPending) focusMemo(); }} />
      </Section>
    </View>}
  </Scroll></View>;
}
