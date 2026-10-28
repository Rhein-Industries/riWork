import { Scroll, Text, View, cleanup, effect, keymap, signal, type YNode } from "nsh";
import { Button, Input } from "nsh/base";
import { Accounts, ProjectSettings as NativeProjectSettings, Settings, Store, type AccountsDto, type ProjectSettingsDto } from "riwork/native";
import { MemoEditor } from "../Files";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
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
  const error = signal(""), pending = signal(false), accountPending = signal(false), typedModel = signal("");
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
  effect(() => {
    const id = props.projectId();
    if (id !== current) { current = id; ready.set(false); snapshot.set(null); folderName.set(""); typedModel.set(""); error.set(""); }
    if (shown()) {
      void refresh();
      if (accounts() && !accounts()!.listed && !accountRefreshStarted) { accountRefreshStarted = true; void refreshAccounts(); }
    }
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
    const form = draft(); if (!form || pending() || snapshot()?.project.id !== props.projectId()) return;
    const request = form.request();
    await run(async () => {
      const result = await Store.updateProjectMetadata(request.id, request.name, request.folderId);
      form.saved(request, result);
    }, "Settings saved");
  }
  function saveAll() { memoControl?.save(); void save(); }
  effect(() => { if (shown()) return keymap.bind("mod+s", saveAll); });
  async function addFolder() {
    const form = draft(); if (!form) return;
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
    if (!alive || pending()) return;
    const typed = typedModel(), id = props.projectId();
    await chooseOrchestrator({ mode: "chat", provider: orchestrator().provider, model: typed });
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
  return <Scroll testID="project-settings-panel" ref={(node) => { scroll = node; }} style={{ flex: 1, minHeight: 0 }}>
    {!ready() ? <Text style={textStyle(theme)}>{error() || (props.projectId() ? "Reading project settings…" : "Select a local project to edit its settings.")}</Text> : <View style={{ padding: 12, gap: 12, minWidth: 0 }}>
      <View style={{ gap: 4 }}><Text role="header" style={textStyle(theme, 17)}>Project settings</Text><Text style={textStyle(theme, 12)}>{snapshot()!.project.name}</Text><Text testID="project-settings-badge" style={textStyle(theme, 10, true)}>{draft()!.pending ? "Unsaved" : "Local project"}</Text></View>
      {note(error(), "project-settings-error")}
      {note(snapshot()!.settings.error, "project-settings-preferences-error")}
      <Section number="01" title="Identity" theme={theme}><Input testID="project-settings-name" ref={(node) => { nameInput = node; }} accessibilityLabel="Project name" value={draft()!.name} onChange={(name) => { draft()!.editName(name); error.set(""); redraw(); }} onSubmit={() => void save()} style={fieldStyle()} /></Section>
      <Section number="02" title="Virtual folder" description="Group projects in the browser. Files stay in their current locations." theme={theme}>
        <ChoiceRow id="project-settings-folders" label="Virtual folder" value={draft()!.folderId ?? "project-folder-unfiled"} disabled={pending()} theme={theme}
          choices={[{ id: "project-folder-unfiled", label: "Unfiled" }, ...paths()]}
          onChange={(id) => { draft()!.chooseFolder(id === "project-folder-unfiled" ? null : id); error.set(""); redraw(); }} />
        {draft()!.folderId && <Text style={textStyle(theme, 10, true)}>{`New subfolders go under ${paths().find((folder) => folder.id === draft()!.folderId)?.label ?? "the selected folder"}`}</Text>}
        <Input testID="project-settings-folder-name" accessibilityLabel={draft()!.folderId ? "New subfolder" : "New virtual folder"} placeholder={draft()!.folderId ? "New subfolder" : "New virtual folder"} value={folderName()} onChange={(value) => folderName.set(value)} onSubmit={() => void addFolder()} style={fieldStyle()} />
        <Button testID="project-settings-create-folder" disabled={pending()} onPress={() => void addFolder()} style={buttonStyle(theme)}><Text style={textStyle(theme)}>{draft()!.folderId ? "Add subfolder" : "Add folder"}</Text></Button>
      </Section>
      <Section number="03" title="Codex account" description="New Codex sessions use this choice. Running sessions keep their account. Selection saves immediately." theme={theme}>
        <ChoiceRow id="project-codex-accounts" label="Codex account" value={accountId()} choices={snapshot()!.accounts.map((choice) => ({ id: accountKey(choice), label: choice.label, disabled: !choice.available, description: choice.reason ?? undefined }))} disabled={pending()} onChange={chooseAccount} theme={theme} />
        {snapshot()!.selectedAccountUnavailable && note("Selected account unavailable. Choose another before starting Codex.", "project-codex-unavailable")}
        {note(accounts()?.error, "project-codex-error")}
        <Button testID="project-codex-refresh" disabled={accountPending() || accounts()?.refreshing} onPress={() => void refreshAccounts()} style={buttonStyle(theme)}><Text style={textStyle(theme)}>{accountPending() || accounts()?.refreshing ? "Checking accounts…" : "Refresh accounts"}</Text></Button>
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
      <View style={{ gap: 6 }}><Button testID="project-settings-save" disabled={pending()} onPress={() => void save()} style={{ ...buttonStyle(theme), borderColor: draft()!.pending ? theme().palette.cyan : theme().palette.divider }}><Text style={textStyle(theme)}>{pending() ? "Saving…" : "Save project"}</Text></Button><Text testID="project-settings-save-status" style={textStyle(theme, 10, true)}>{draft()!.message()}</Text></View>
      <Section number="06" title="Locations" theme={theme}>
        <Text selectable={true} style={{ ...textStyle(theme), fontFamily: theme().monoFamily }}>{snapshot()!.project.root}</Text>
        <Text style={textStyle(theme)}>{`Repositories · ${snapshot()!.project.repositoryRoots.length}`}</Text>
        {snapshot()!.project.repositoryRoots.length ? snapshot()!.project.repositoryRoots.map((root) => <Text selectable={true} style={{ ...textStyle(theme, 10), fontFamily: theme().monoFamily }}>{root}</Text>) : <Text style={textStyle(theme, 10, true)}>No Git repositories detected</Text>}
        <Text selectable={true} style={textStyle(theme, 10, true)}>{`Project ID ${snapshot()!.project.id}`}</Text>
      </Section>
      <Section number="07" title="Memo" description="Your own Markdown notes for this project, saved as you type and shared with RiWork on your phone. Type /memo in one of its chats to insert them." theme={theme}>
        <MemoEditor projectId={props.projectId} theme={theme} visible={shown} bindSaveKey={false} onControl={(control) => { memoControl = control; if (control && revealPending) focusMemo(); }} />
      </Section>
    </View>}
  </Scroll>;
}
