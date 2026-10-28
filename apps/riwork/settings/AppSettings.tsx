import { Scroll, Text, cleanup, effect, keymap, signal, untracked, type JSX, type YNode } from "nsh";
import { Button, Input, View, SettingsFocus, settingsFocusScope } from "../SettingsFocus";
import { Accounts, ApiKey, Appearance, OrcaImport, Settings, type AccountsDto, type SettingsDto, type SettingsPatch } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";
import { ChoiceRow, Section, StepperRow, ToggleRow, buttonStyle, defaultTheme, textStyle, type Theme } from "./SettingsControls";
import { SETTINGS_SECTIONS, modelIdError, sectionColumns, settingsLayout } from "./settings_model";
import { ThemePicker, createPaletteCatalog, type PaletteCatalogSource } from "./ThemePicker";

interface Subscription { close(): void }
interface Delivery<T> { rev: number; data: T }
export interface ApiKeyState { saved?: boolean | null; error?: string | null }
export interface ImportReceipt { source: string; projectCount: number; folderCount: number; worktreeCount: number; completedAt: number }
export interface ImportPreview { id: string; source: string; projectCount: number; folderCount: number; worktreeCount: number; alreadyImported?: ImportReceipt | null; recordable: boolean; warnings: string[] }
/** Injectable adapters let headless fixtures test the forms without real credentials or account discovery. */
export interface AppSettingsServices {
  appearance?: PaletteCatalogSource;
  settings: {
    get(): Promise<SettingsDto>; subscribe(listener: (event: Delivery<SettingsDto>) => void): Subscription;
    set(patch: SettingsPatch): Promise<SettingsDto>; changeTextSize(action: string): Promise<SettingsDto>;
    setEnabled(enabled: boolean): Promise<SettingsDto>; setVisible(kind: string, value: boolean): Promise<SettingsDto>;
    setSide(kind: string, side: string): Promise<SettingsDto>; moveItem(kind: string, up: boolean): Promise<SettingsDto>;
    resetStatusBar(): Promise<SettingsDto>; setUsageMode(mode: string): Promise<SettingsDto>;
  };
  accounts: {
    list(): Promise<AccountsDto>; subscribe(listener: (event: Delivery<AccountsDto>) => void): Subscription;
    refresh(): Promise<AccountsDto>; selectDefault(id: string | null): Promise<AccountsDto>;
  };
  apiKey: {
    status(): Promise<ApiKeyState>; refresh(): Promise<ApiKeyState>; subscribe(listener: (event: Delivery<ApiKeyState>) => void): Subscription;
    save(paste: string): Promise<ApiKeyState>; remove(): Promise<ApiKeyState>;
  };
  orcaImport: { preview(): Promise<ImportPreview>; run(id: string): Promise<ImportReceipt> };
}
const nativeServices: AppSettingsServices = { settings: Settings, accounts: Accounts, apiKey: ApiKey, orcaImport: OrcaImport, appearance: Appearance as unknown as PaletteCatalogSource };

export interface AppSettingsControl { revealApiKey(): void }
export interface AppSettingsProps {
  theme?: () => InterfaceTheme; visible?: () => boolean; settings?: () => SettingsDto | null;
  computerUse?: JSX.Element; remote?: JSX.Element;
  computerUseFocus?: () => { initialTarget(): YNode | null | undefined } | null;
  services?: AppSettingsServices; onError?: (reason: unknown) => void;
  onNotice?: (message: string) => void;
  onControl?: (control: AppSettingsControl) => void | (() => void);
}

/** The global Settings form owns drafts only. Every durable edit is a narrow native mutation. */
export function AppSettings(props: AppSettingsProps) {
  const theme = props.theme ?? defaultTheme, services = props.services ?? nativeServices;
  const focus = settingsFocusScope();
  const local = signal<SettingsDto | null>(null), accounts = signal<AccountsDto | null>(null), error = signal("");
  const apiKey = signal<ApiKeyState>({ saved: null, error: null }), apiPaste = signal(""), apiError = signal(""), apiPending = signal(false), apiFocused = signal(false);
  const modelInput = signal(""), modelError = signal("");
  const width = signal(0), scrollY = signal(0), accountFocus = signal<string | null>(null), textFocused = signal(false);
  const themeFocus = signal<string | null>(null), themeNodes = new Map<string, YNode>();
  const preview = signal<ImportPreview | null>(null), receipt = signal<ImportReceipt | null>(null), importPending = signal(""), importError = signal("");
  let alive = true, initializedImport = false, accountsRev = -1, settingsRev = -1, apiRev = -1, wasVisible = false, autoAccountsAttempted = false;
  let apiInput: YNode | undefined, page: YNode | undefined, apiRow: YNode | undefined;
  let revealPending = false, revealTimer: ReturnType<typeof setTimeout> | undefined;
  let initialFocusPending = false, initialFocusTimer: ReturnType<typeof setTimeout> | undefined;
  let mutation = Promise.resolve();
  const settings = () => props.settings?.() ?? local();
  const visible = () => props.visible?.() ?? true;
  const paletteCatalog = createPaletteCatalog(services.appearance, visible);
  const fail = (reason: unknown) => { if (alive) { error.set(String(reason)); props.onError?.(reason); } };
  function enqueue(action: () => Promise<SettingsDto>) {
    mutation = mutation.then(async () => { const next = await action(); if (alive) { local.set(next); error.set(""); } }).catch(fail);
  }
  const patch = (value: SettingsPatch) => enqueue(() => services.settings.set(value));
  const subscriptions = [
    services.settings.subscribe((event) => { if (alive && event.rev > settingsRev) { settingsRev = event.rev; local.set(event.data); } }),
    services.accounts.subscribe((event) => { if (alive && event.rev > accountsRev) { accountsRev = event.rev; accounts.set(event.data); } }),
    services.apiKey.subscribe((event) => { if (alive && event.rev > apiRev) { apiRev = event.rev; apiKey.set(event.data); } }),
  ];
  void services.settings.get().then((value) => { if (alive && settingsRev < 0) local.set(value); }).catch(fail);
  void services.accounts.list().then((value) => { if (alive && accountsRev < 0) accounts.set(value); }).catch(fail);
  cleanup(() => { alive = false; clearTimeout(revealTimer); clearTimeout(initialFocusTimer); for (const subscription of subscriptions) subscription.close(); themeNodes.clear(); });
  function initialTarget() {
    if (props.computerUseFocus) {
      const control = props.computerUseFocus();
      if (!control) return undefined;
      const target = control.initialTarget();
      if (target !== null) return target;
    }
    return themeNodes.get(settings()?.theme ?? "native");
  }
  function scheduleInitialFocus() { untracked(() => {
    if (!alive || !initialFocusPending || revealPending || !visible()) return;
    if (focus.containsFocus()) { initialFocusPending = false; clearTimeout(initialFocusTimer); return; }
    if (!initialTarget()) return;
    clearTimeout(initialFocusTimer);
    // Workspace first focuses the pane body. The panel's own initial target
    // follows it once Cua has settled and the target has a native ref.
    initialFocusTimer = setTimeout(() => {
      if (!alive || !initialFocusPending || revealPending || !visible()) return;
      if (focus.containsFocus()) { initialFocusPending = false; return; }
      const node = initialTarget();
      if (!node) return;
      initialFocusPending = false;
      node.focus();
    }, 0);
  }); }
  // A check may finish, or the Cua ref may mount, after the show notification.
  effect(() => { initialTarget(); focus.containsFocus(); scheduleInitialFocus(); });
  function scheduleReveal() {
    if (!alive || !revealPending || !visible() || !page || !apiRow || !apiInput) return;
    clearTimeout(revealTimer);
    revealTimer = setTimeout(() => {
      void (async () => {
        if (!alive || !revealPending || !visible()) return;
        const row = apiRow, field = apiInput, scroll = page;
        const outer = await scroll?.measure(), target = await row?.measure();
        if (!alive || !visible() || row !== apiRow || field !== apiInput || !outer || !target) return;
        scroll?.scrollTo({ x: 0, y: Math.max(0, scrollY() + target.pagePosition.y - outer.pagePosition.y), animated: false });
        field?.focus(); revealPending = false;
      })().catch(() => { /* A replaced row's next ref/layout retries the reveal. */ });
    }, 0);
  }

  async function refreshAccounts() {
    if (accounts()?.refreshing) return;
    try { const value = await services.accounts.refresh(); if (alive) accounts.set(value); }
    catch (reason) { fail(reason); }
  }
  async function selectAccount(id: string | null, available: boolean) {
    if (!available) { error.set("This account is unavailable. Refresh accounts or choose another."); return; }
    try { const value = await services.accounts.selectDefault(id); if (alive) { accounts.set(value); error.set(""); } }
    catch (reason) { fail(reason); }
  }
  async function refreshKey() {
    try { const value = await services.apiKey.refresh(); if (alive) apiKey.set(value); }
    catch (reason) { if (alive) apiError.set(String(reason)); }
  }
  async function saveKey() {
    if (apiPending()) return;
    apiPending.set(true); apiError.set("");
    try { const value = await services.apiKey.save(apiPaste()); if (alive) { apiKey.set(value); apiPaste.set(""); } }
    catch (reason) { if (alive) apiError.set(String(reason)); }
    finally { if (alive) apiPending.set(false); }
  }
  async function removeKey() {
    if (apiPending()) return;
    apiPending.set(true); apiError.set("");
    try { const value = await services.apiKey.remove(); if (alive) apiKey.set(value); }
    catch (reason) { if (alive) apiError.set(String(reason)); }
    finally { if (alive) apiPending.set(false); }
  }
  async function previewImport() {
    if (importPending()) return;
    initializedImport = true; importPending.set("Reading Orca import preview…"); importError.set(""); preview.set(null);
    try { const value = await services.orcaImport.preview(); if (alive) { preview.set(value); receipt.set(value.alreadyImported ?? null); } }
    catch (reason) { if (alive) importError.set(String(reason)); }
    finally { if (alive) importPending.set(""); }
  }
  async function runImport() {
    const selected = preview();
    if (importPending() || receipt() || !selected?.recordable) return;
    importPending.set("Importing Orca projects and worktrees…"); importError.set("");
    try { const value = await services.orcaImport.run(selected.id); if (alive) { receipt.set(value); props.onNotice?.("Orca import completed"); } }
    catch (reason) { if (alive) { preview.set(null); importError.set(String(reason)); } }
    finally { if (alive) importPending.set(""); }
  }
  // Refresh only on a visibility transition. Accounts discovery remains explicit
  // after its first catalog, and Orca preview runs once per panel owner.
  effect(() => {
    const shown = visible();
    if (shown === wasVisible) return;
    wasVisible = shown;
    if (!shown) { initialFocusPending = false; clearTimeout(initialFocusTimer); return; }
    initialFocusPending = !revealPending;
    scheduleInitialFocus();
    scheduleReveal();
    void refreshKey();
    if (!initializedImport) void previewImport();
  });
  effect(() => {
    if (!visible()) { autoAccountsAttempted = false; return; }
    if (!autoAccountsAttempted && accounts() && !accounts()!.listed && !accounts()!.refreshing) { autoAccountsAttempted = true; void refreshAccounts(); }
  });
  cleanup(props.onControl?.({ revealApiKey: () => { revealPending = true; initialFocusPending = false; clearTimeout(initialFocusTimer); scheduleReveal(); } }) ?? (() => {}));
  // Secure input owns ordinary editing. Explicit copy/cut bindings ensure the
  // masked paste never leaves through the window's menu equivalents.
  effect(() => { if (!visible() || !apiFocused()) return; const bindings = [keymap.bind("mod+c", () => {}, { when: "always" }), keymap.bind("mod+x", () => {}, { when: "always" })]; return () => { for (const unbind of bindings) unbind(); }; });
  effect(() => {
    if (!visible() || !accountFocus()) return;
    const activate = () => { const account = accounts()?.accounts.find((item) => item.id === accountFocus()); if (account) void selectAccount(account.system ? null : account.id, account.available); };
    const bindings = [keymap.bind("enter", activate), keymap.bind("space", activate)];
    return () => { for (const unbind of bindings) unbind(); };
  });
  effect(() => {
    if (!visible() || !textFocused()) return;
    const bindings: (() => void)[] = [];
    for (const [key, action] of [["up", "bigger"], ["right", "bigger"], ["=", "bigger"], ["shift+=", "bigger"], ["down", "smaller"], ["left", "smaller"], ["-", "smaller"], ["0", "reset"]] as const) bindings.push(keymap.bind(key, () => enqueue(() => services.settings.changeTextSize(action))));
    bindings.push(keymap.bind("m", () => patch({ uiTextMatchesTerminal: !settings()?.uiTextMatchesTerminal })));
    return () => { for (const unbind of bindings) unbind(); };
  });
  effect(() => {
    if (!visible() || !themeFocus()) return;
    const move = (step: number) => {
      const choices = paletteCatalog.ids();
      if (!choices.length) return;
      const index = choices.indexOf(themeFocus() ?? "");
      themeNodes.get(choices[(index + step + choices.length) % choices.length])?.focus();
    };
    return keymap.bind({ left: () => move(-1), up: () => move(-1), right: () => move(1), down: () => move(1) });
  });

  const muted = () => textStyle(theme, 10, true);
  const button = (id: string, label: string, action: () => void, disabled = false) => <Button testID={id} accessibilityLabel={label} disabled={disabled} onPress={action} style={buttonStyle(theme)} focusStyle={{ borderColor: theme().palette.focus }}><Text style={textStyle(theme, 10)}>{label}</Text></Button>;
  const note = (text: string | null | undefined, id?: string, warning = false) => !!text && <Text testID={id} selectable={true} style={{ ...muted(), color: warning ? theme().palette.gold : theme().palette.muted }}>{text}</Text>;
  function accountBody() {
    return <View style={{ gap: 8 }}>
      <ToggleRow id="codex-subscription-balancing" label="Balance Codex subscriptions" checked={settings()!.codexSubscriptionBalancing} onChange={(value) => patch({ codexSubscriptionBalancing: value })} theme={theme}
        description="New automatic Codex sessions use the saved account with the most quota left across its limits. Explicit accounts, project account choices and existing sessions stay fixed. Quota is checked before launch; missing, stale or exhausted accounts are excluded. If none qualify, choose an account explicitly or turn this off." />
      {note("Choose the account for new Codex sessions when subscription balancing is off, or the preferred account when balanced quotas tie. Running sessions keep their account. A project can override this in Project Settings.")}
      {button("refresh-codex-accounts", accounts()?.refreshing ? "Checking accounts…" : "Refresh accounts", () => void refreshAccounts(), !!accounts()?.refreshing)}
      <View role="radiogroup" accessibilityLabel="Default Codex account" style={{ gap: 6 }}>
        {accounts()?.accounts.map((account) => {
          const selected = () => account.system ? !accounts()?.selectedId : accounts()?.selectedId === account.id;
          return <View testID={`codex-account-${account.id}`} role="radio" focusable={account.available} accessibilityLabel={account.label} accessibilityState={{ checked: selected(), selected: selected(), disabled: !account.available }}
            disabled={!account.available} onFocus={() => { if (alive) accountFocus.set(account.id); }} onBlur={() => { if (alive) accountFocus.set(null); }} onPress={() => { if (!alive) return; if (account.available) accountFocus.set(account.id); void selectAccount(account.system ? null : account.id, account.available); }} style={{ paddingHorizontal: space(6, theme().scale), paddingVertical: space(7, theme().scale), borderRadius: 7, gap: 3, opacity: account.available ? 1 : 0.6, backgroundColor: selected() ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
            <Text style={textStyle(theme)}>{account.label}</Text><Text style={muted()}>{account.detail}</Text>
            <Text style={{ ...muted(), color: !account.available ? theme().palette.gold : selected() ? theme().palette.cyan : theme().palette.muted }}>{!account.available ? "Unavailable" : selected() ? "Selected" : "Use account"}</Text>
          </View>;
        })}
      </View>
      {note(accounts()?.selectedUnavailable ? "Your selected account is unavailable. Refresh accounts or choose another before starting Codex." : null, "settings-account-unavailable", true)}
      {note(accounts()?.error, "settings-accounts-error", true)}
      {note(accounts()?.fromCache ? "Showing saved accounts while Orca is unavailable." : null)}
    </View>;
  }
  function appearanceBody() {
    return <View style={{ gap: 8 }}>
      <ThemePicker catalog={paletteCatalog} selected={() => settings()!.theme} theme={theme} onChoose={(id) => patch({ theme: id })}
        onNode={(id, node) => { themeNodes.set(id, node); scheduleInitialFocus(); }}
        onFocus={(id) => { if (alive) themeFocus.set(id); }} onBlur={(id) => { if (alive && themeFocus() === id) themeFocus.set(null); }} />
      {settings()!.theme === "ghostty" && <ToggleRow id="terminal-colors" label="Use RiWork terminal colors" checked={settings()!.useRiworkColors} onChange={(value) => patch({ useRiworkColors: value })} theme={theme} />}
      {settings()!.theme === "native" && <ToggleRow id="terminal-colors" label="Terminals match the theme" checked={settings()!.nativeTerminalColors} onChange={(value) => patch({ nativeTerminalColors: value })} theme={theme} />}
      <ChoiceRow id="interface-font" label="Interface font" value={`interface-font-${settings()!.interfaceFont}`} choices={[{ id: "interface-font-system", label: "System (SF Pro)" }, { id: "interface-font-system_mono", label: "System + monospace accents" }]} onChange={(value) => patch({ interfaceFont: value === "interface-font-system" ? "system" : "system_mono" })} theme={theme} />
      <ToggleRow id="panel-tab-icons" label="Icons instead of labels" checked={settings()!.panelTabIcons} onChange={(value) => patch({ panelTabIcons: value })} theme={theme} />
      <ChoiceRow id="tab-close-behavior" label="When closing a tab" description="Ask offers Detach, which keeps the session running, and Exit, which ends it. Workers always detach. This Mac only." value={`tab-close-${settings()!.tabCloseBehavior}`} choices={[{ id: "tab-close-ask", label: "Ask" }, { id: "tab-close-detach", label: "Detach" }, { id: "tab-close-exit", label: "Exit" }]} onChange={(value) => patch({ tabCloseBehavior: value === "tab-close-ask" ? "ask" : value === "tab-close-detach" ? "detach" : "exit" })} theme={theme} />
      <View testID="ui-text-size" focusable={true} accessibilityLabel="Interface text size" onFocus={() => { if (alive) textFocused.set(true); }} onBlur={() => { if (alive) textFocused.set(false); }} style={{ gap: 6, paddingVertical: 8 }}>
        <Text style={textStyle(theme)}>Interface text size</Text><View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Button testID="ui-text-smaller" accessibilityLabel="Smaller text" disabled={settings()!.uiTextMatchesTerminal || settings()!.uiTextSize <= 9} onPress={() => enqueue(() => services.settings.changeTextSize("smaller"))} style={buttonStyle(theme)}><Text style={textStyle(theme)}>−</Text></Button>
          <Text testID="ui-text-points" style={{ ...textStyle(theme), opacity: settings()!.uiTextMatchesTerminal ? 0.5 : 1 }}>{`${settings()!.uiTextMatchesTerminal ? settings()!.text.points : settings()!.uiTextSize} pt`}</Text>
          <Button testID="ui-text-bigger" accessibilityLabel="Bigger text" disabled={settings()!.uiTextMatchesTerminal || settings()!.uiTextSize >= 24} onPress={() => enqueue(() => services.settings.changeTextSize("bigger"))} style={buttonStyle(theme)}><Text style={textStyle(theme)}>+</Text></Button>
          {!settings()!.uiTextMatchesTerminal && settings()!.uiTextSize !== 11 && <Button testID="ui-text-reset" accessibilityLabel="Actual size" onPress={() => enqueue(() => services.settings.changeTextSize("reset"))} style={buttonStyle(theme)}><Text style={textStyle(theme)}>Reset</Text></Button>}
        </View>
        <ToggleRow id="ui-text-match-terminal" label="Match terminal" checked={settings()!.uiTextMatchesTerminal} onChange={(value) => patch({ uiTextMatchesTerminal: value })} theme={theme} />
        {note("Panels, tabs, menus and status bar. Matches Ghostty's font-size when enabled. ⌘+ bigger ⌘− smaller ⌘0 reset outside a terminal.")}
      </View>
    </View>;
  }
  function apiKeyRow() {
    return <View testID="claude-api-key" ref={(node) => { apiRow = node; scheduleReveal(); }} onLayout={scheduleReveal} style={{ paddingVertical: 8, gap: 6 }}>
      <Text style={textStyle(theme)}>Anthropic API key</Text>
      <Text testID="claude-api-key-state" style={{ ...muted(), color: apiKey().saved === true ? theme().palette.cyan : apiKey().saved == null ? theme().palette.gold : theme().palette.muted }}>{apiKey().saved === true ? "API key saved" : apiKey().saved === false ? "No API key saved" : "The Keychain cannot say whether an API key is saved"}</Text>
      {!settings()!.claudeChatIgnoresApiKey && note("API key billing uses the key saved here. With no saved key, Claude uses a key your shell exports, or your Claude sign-in.")}
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
        <Input testID="claude-api-key-input" accessibilityLabel="Anthropic API key" ref={(node) => { apiInput = node; scheduleReveal(); }} masked={true} autoCapitalize="none" autoCorrect={false} placeholder="Paste an Anthropic API key" value={apiPaste()} onChange={(value) => { if (alive) apiPaste.set(value); }} onFocus={() => { if (alive) apiFocused.set(true); }} onBlur={() => { if (alive) apiFocused.set(false); }} onSubmit={() => { if (alive) void saveKey(); }} style={{ width: 220, color: theme().palette.text, backgroundColor: theme().palette.bg, padding: 8 }} />
        {button("claude-api-key-save", "Save", () => void saveKey(), apiPending())}
        {apiKey().saved === true && button("claude-api-key-remove", "Remove", () => void removeKey(), apiPending())}
      </View>
      {note(apiError() || apiKey().error, "claude-api-key-error", true)}
    </View>;
  }
  function agentBody() {
    const s = settings()!;
    return <View style={{ gap: 8 }}>
      <ToggleRow id="agent-inline-mode" label="Keep agent transcripts in scrollback (inline mode)" description="New Codex, Grok, and Claude sessions draw on the terminal's main screen, so the whole conversation stays in the terminal's scrollback. Off runs them full screen. Running sessions keep their mode until they restart." checked={s.agentInlineMode} onChange={(value) => patch({ agentInlineMode: value })} theme={theme} />
      <ToggleRow id="agent-terminals-unrestricted" label="Agent terminals run unrestricted" description="Codex, Claude, Grok and Muse terminals start without approval prompts. Off, they ask before running commands and editing files. Running sessions keep their mode." checked={s.agentTerminalsUnrestricted} onChange={(value) => patch({ agentTerminalsUnrestricted: value })} theme={theme} />
      <ToggleRow id="dictation-mic" label="Show microphone buttons for dictation" description="Covers chat message boxes on this Mac (or ⌃⌥D) and microphone buttons on a paired iPhone. Speech is recognized on the device you speak to. Off hides them all." checked={s.dictationMic} onChange={(value) => patch({ dictationMic: value })} theme={theme} />
      <ChoiceRow id="claude-billing" label="Claude chats bill" description="Subscription uses your Claude sign-in without ANTHROPIC_API_KEY. API key bills the key saved below, or uses Claude as installed when no key is saved. Each chat keeps its choice. Claude terminal tabs are not affected." value={s.claudeChatIgnoresApiKey ? "claude-billing-subscription" : "claude-billing-api-key"} choices={[{ id: "claude-billing-subscription", label: "Subscription" }, { id: "claude-billing-api-key", label: "API key" }]} onChange={(value) => patch({ claudeChatIgnoresApiKey: value === "claude-billing-subscription" })} theme={theme} />
      {apiKeyRow()}
      <ToggleRow id="dashboard-haiku-summaries" label="Haiku dashboard summaries" description="Summarizes observed work for the dashboard using the saved Anthropic API key. Recent commands, file paths and messages from every provider's chats are sent to Anthropic. API usage is billed separately; decisions remain provider requests." checked={s.dashboardHaikuSummaries} onChange={(value) => patch({ dashboardHaikuSummaries: value })} theme={theme} />
      <ChoiceRow id="orchestrator-mode" label="Orchestrator runs as" description="Automatic lets RiWork choose a native chat. Terminal uses Codex in a terminal tab. Chat uses the provider and model below." value={`orchestrator-mode-${s.orchestratorMode}`} choices={[{ id: "orchestrator-mode-automatic", label: "Automatic" }, { id: "orchestrator-mode-terminal", label: "Terminal" }, { id: "orchestrator-mode-chat", label: "Chat" }]} onChange={(value) => patch({ orchestratorMode: value === "orchestrator-mode-automatic" ? "automatic" : value === "orchestrator-mode-terminal" ? "terminal" : "chat" })} theme={theme} />
      {s.orchestratorMode === "automatic" && note("RiWork chooses a Muse chat on Wonder Full, or a Codex chat on GPT-6.1-Sol when Muse cannot start.", "orchestrator-automatic-note")}
      {s.orchestratorMode === "chat" && <>
        <ChoiceRow id="orchestrator-provider" label="Chat provider" description="Choosing another provider starts it on that provider's default model." value={`orchestrator-provider-${s.orchestratorChatProvider}`} choices={[{ id: "orchestrator-provider-codex", label: "Codex" }, { id: "orchestrator-provider-claude", label: "Claude" }, { id: "orchestrator-provider-muse", label: "Muse" }]} onChange={(value) => patch({ orchestratorChatProvider: value === "orchestrator-provider-codex" ? "codex" : value === "orchestrator-provider-claude" ? "claude" : "muse", orchestratorChatModel: null })} theme={theme} />
        <ChoiceRow id="orchestrator-model" label="Chat model" value={`orchestrator-model-${s.modelChoices.find((choice) => choice.model === s.orchestratorChatModel)?.id ?? "default"}`} choices={s.modelChoices.map((choice) => ({ id: `orchestrator-model-${choice.id}`, label: choice.label }))} onChange={(value) => patch({ orchestratorChatModel: s.modelChoices.find((choice) => `orchestrator-model-${choice.id}` === value)?.model ?? null })} theme={theme} />
        <View style={{ gap: 6 }}><Text style={textStyle(theme)}>Other model</Text><Input testID="orchestrator-other-model" accessibilityLabel="Other model ID" placeholder="Other model ID" value={modelInput()} onChange={(value) => modelInput.set(value)} onSubmit={() => { const invalid = modelIdError(modelInput()); modelError.set(invalid ?? ""); if (!invalid) { patch({ orchestratorChatModel: modelInput().trim() }); modelInput.set(""); } }} />{note(modelError(), "orchestrator-model-error", true)}</View>
      </>}
      {note(s.orchestratorUnreadableNote, "orchestrator-unreadable-note", true)}
      {note("Applies to orchestrators created from now on, except a project's whose Project Settings choose otherwise. Existing orchestrators keep their mode, provider and model until closed and created again.")}
      <Text style={{ ...textStyle(theme), color: theme().palette.cyan }}>Delegation</Text>
      <ToggleRow id="delegation-spread-work" label="Spread work" description="Orchestrators fan independent tasks out to several worker shells at once instead of working through them one by one. Off, they keep to one worker at a time unless asked." checked={s.delegation.spreadWork} onChange={(value) => patch({ delegationPatch: { spreadWork: value } })} theme={theme} />
      <StepperRow id="delegation-parallel-shells" label="Parallel worker shells" value={s.delegation.parallelShells} minimum={1} maximum={10} onChange={(value) => patch({ delegationPatch: { parallelShells: value } })} theme={theme} />
      {(["design", "workhorse"] as const).map((work) => {
        const choices = work === "design" ? s.delegation.designChoices : s.delegation.workhorseChoices;
        const route = s.delegation[work], selected = choices.find((choice) => choice.route.provider === route.provider && choice.route.model === route.model);
        return <ChoiceRow id={`delegation-${work}`} label={work === "design" ? "Design work" : "Workhorse work"} value={`delegation-${work}-${selected?.id ?? "saved"}`} choices={choices.map((choice) => ({ id: `delegation-${work}-${choice.id}`, label: choice.label }))} onChange={(value) => { const route = choices.find((choice) => `delegation-${work}-${choice.id}` === value)?.route; if (route) patch({ delegationPatch: { [work]: route } }); }} theme={theme} />;
      })}
      {note(s.delegation.unreadableNote, "delegation-unreadable-note", true)}
    </View>;
  }
  function statusBody() {
    return <View style={{ gap: 8 }}>
      <ToggleRow id="settings-status-enabled" label="Show status bar" checked={settings()!.statusBar.enabled} onChange={(value) => enqueue(() => services.settings.setEnabled(value))} theme={theme} />
      {button("settings-status-reset", "Reset defaults", () => enqueue(() => services.settings.resetStatusBar()))}
      <ChoiceRow id="status-usage-mode" label="Account usage shows" value={`status-usage-${settings()!.statusBar.usageMode}`} choices={[{ id: "status-usage-current_chat", label: "Current chat" }, { id: "status-usage-all_accounts", label: "All accounts" }]} onChange={(value) => enqueue(() => services.settings.setUsageMode(value.slice(13)))} theme={theme} />
      {settings()!.statusBar.items.map((item) => {
        const sameSide = settings()!.statusBar.items.filter((peer) => peer.side === item.side), index = sameSide.findIndex((peer) => peer.kind === item.kind);
        return <View testID={`settings-status-item-${item.kind}`} style={{ paddingVertical: 5, borderBottomWidth: 1, borderColor: theme().palette.divider, gap: 4 }}>
          <ToggleRow id={`settings-status-visible-${item.kind}`} label={item.label} checked={item.enabled} onChange={(value) => enqueue(() => services.settings.setVisible(item.kind, value))} theme={theme} />
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 5 }}>
            {button(`settings-status-side-${item.kind}`, `Move to the ${item.side === "left" ? "right" : "left"} side`, () => enqueue(() => services.settings.setSide(item.kind, item.side === "left" ? "right" : "left")))}
            {button(`settings-status-earlier-${item.kind}`, "Move earlier on this side", () => enqueue(() => services.settings.moveItem(item.kind, true)), index <= 0)}
            {button(`settings-status-later-${item.kind}`, "Move later on this side", () => enqueue(() => services.settings.moveItem(item.kind, false)), index >= sameSide.length - 1)}
          </View>
        </View>;
      })}
    </View>;
  }
  function importBody() {
    const counts = () => receipt() ?? preview();
    return <View testID="orca-import" style={{ gap: 8 }}>
      <Text style={{ ...textStyle(theme), color: importPending() ? theme().palette.gold : theme().palette.cyan }}>{importPending() ? "Working" : receipt() ? "Imported" : "One-time import"}</Text>
      {counts() && <><Text selectable={true} style={muted()}>{`ORCA CLI · ${counts()!.source}`}</Text><Text style={textStyle(theme)}>{`${counts()!.projectCount} projects${counts()!.folderCount > 0 ? ` · ${counts()!.folderCount} folders` : ""} · ${counts()!.worktreeCount} worktrees`}</Text></>}
      {!!preview()?.warnings.length && <Scroll style={{ maxHeight: 120 }}>{preview()!.warnings.map((warning) => note(warning, undefined, true))}</Scroll>}
      {note(importPending(), "orca-import-pending")}{note(importError(), "orca-import-error", true)}
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
        {button("orca-import-preview", receipt() ? "Check import" : "Preview import", () => void previewImport(), !!importPending())}
        {!receipt() && preview()?.recordable && button("orca-import-confirm", preview()!.projectCount + preview()!.worktreeCount > 0 ? "Import now" : "Finish import", () => void runImport(), !!importPending())}
      </View>
    </View>;
  }
  function body(number: string) {
    if (!settings()) return <Text style={muted()}>Loading settings…</Text>;
    switch (number) {
      case "01": return props.computerUse ?? <Text style={muted()}>Computer use settings are unavailable here.</Text>;
      case "02": return accountBody();
      case "03": return appearanceBody();
      case "04": return <ToggleRow id="open-preview-on-select" label="Open the preview when a file is selected" description="Select a file in Files to open Preview beside it. You can always open Preview with ⌘⇧P." checked={settings()!.openPreviewOnSelect} onChange={(value) => patch({ openPreviewOnSelect: value })} theme={theme} />;
      case "05": return agentBody();
      case "06": return <ToggleRow id="remember-window-size" label="Remember project window size" description="Reopen projects at their last size. Switching projects keeps the current window size." checked={settings()!.rememberWindowSize} onChange={(value) => patch({ rememberWindowSize: value })} theme={theme} />;
      case "07": return statusBody();
      case "08": return props.remote ?? <Text style={muted()}>Remote settings are unavailable here.</Text>;
      case "09": return importBody();
    }
  }
  return <SettingsFocus.Provider value={focus}><View testID="settings-panel" onLayout={(event) => { if (alive && event.size.width !== width()) width.set(event.size.width); }} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <View style={{ height: space(46, theme().scale), paddingHorizontal: space(12, theme().scale), justifyContent: "center", gap: space(1, theme().scale) }}>
      <View style={{ flexDirection: "row", gap: 10, alignItems: "center" }}><Text role="header" style={{ ...textStyle(theme, 13), flex: 1, fontWeight: "600" }}>Settings</Text></View>
      <Text style={muted()}>All projects · Saved automatically</Text>
    </View>
    <Scroll testID="appearance-settings" ref={(node) => { page = node; scheduleReveal(); }} onScroll={(event) => { if (alive) scrollY.set(event.offset.y); }} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: space(12, theme().scale), alignItems: "center" }}>
      <View testID={`settings-layout-${settingsLayout(width()).kind}`} style={{ width: "100%", maxWidth: settingsLayout(width()).maxWidth, flexDirection: settingsLayout(width()).kind === "wide" ? "row" : "column", gap: settingsLayout(width()).gap }}>
        {sectionColumns(width()).map((column) => <View style={{ flex: 1, minWidth: 0, gap: 0 }}>{column.map((number) => {
          const section = SETTINGS_SECTIONS.find((section) => section.number === number)!;
          return <Section number={section.number} title={section.title} description={section.description} padding={settingsLayout(width()).card} theme={theme}>{body(number)}</Section>;
        })}</View>)}
      </View>
      {note(error() || settings()?.error, "settings-save-error", true)}
      <Text style={{ ...muted(), paddingVertical: 12 }}>Tab to move · Enter or Space to select · Arrows step the text size and worker shells</Text>
    </Scroll>
  </View></SettingsFocus.Provider>;
}
