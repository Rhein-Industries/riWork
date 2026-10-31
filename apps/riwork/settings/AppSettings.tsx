import { Scroll, Text, cleanup, effect, keymap, signal, untracked, type JSX, type YNode } from "nsh";
import { Button, Input, View, SettingsFocus, settingsFocusScope } from "../SettingsFocus";
import { Accounts, ApiKey, Appearance, OrcaImport, Settings, type AccountsDto, type SettingsDto, type SettingsPatch } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";
import { ChoiceRow, Section, StepperRow, ToggleRow, buttonStyle, defaultTheme, textStyle, type Theme } from "./SettingsControls";
import { SETTINGS_SECTIONS, modelIdError, sectionColumns, settingsLayout } from "./settings_model";
import { ThemePicker, createPaletteCatalog, type PaletteCatalogSource } from "./ThemePicker";
import { SpeechModelPicker } from "./SpeechModels";

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
const MODE_LABELS: Record<string, string> = { supervised: "Supervised", auto_edit: "Auto-edit", full: "Full", plan: "Plan" };
/** "Claude · opus · high · fast · Auto-edit". */
export function newChatSummary(defaults: { provider: string; model?: string | null; effort?: string | null; fast: boolean; mode: string }) {
  return [defaults.provider[0].toUpperCase() + defaults.provider.slice(1), defaults.model || "default model", defaults.effort, defaults.fast ? "fast" : null, MODE_LABELS[defaults.mode] ?? defaults.mode]
    .filter(Boolean).join(" · ");
}

export function AppSettings(props: AppSettingsProps) {
  const theme = props.theme ?? defaultTheme, services = props.services ?? nativeServices;
  const focus = settingsFocusScope();
  const local = signal<SettingsDto | null>(null), accounts = signal<AccountsDto | null>(null), error = signal("");
  const apiKey = signal<ApiKeyState>({ saved: null, error: null }), apiPaste = signal(""), apiError = signal(""), apiPending = signal(false), apiFocused = signal(false);
  const modelInput = signal(""), modelError = signal(""), modelFocused = signal(false);
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
      const selected = settings()?.theme ?? "native";
      if (node === themeNodes.get(selected)) themeFocus.set(selected);
      node.focus();
    }, 0);
  }); }
  // The focused palette card is recorded here, not only by its focus event, as the account
  // rows and ChoiceRow record theirs: a click or a programmatic focus need not send one (NSH's
  // headless driver focuses only text inputs), and the arrows, Enter and Space act on it.
  function focusTheme(id: string) {
    const node = themeNodes.get(id);
    if (!alive || !node) return;
    themeFocus.set(id);
    node.focus();
  }
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

  // Refresh and selection also publish to the revisioned subscription. Their direct
  // results carry no revision, so they only stand in until the first delivery: a slow
  // refresh must not roll back a selection delivered after it.
  async function refreshAccounts() {
    if (accounts()?.refreshing) return;
    try { const value = await services.accounts.refresh(); if (alive && accountsRev < 0) accounts.set(value); }
    catch (reason) { fail(reason); }
  }
  async function selectAccount(id: string | null, available: boolean) {
    if (!available) { error.set("This account is unavailable. Refresh accounts or choose another."); return; }
    try { const value = await services.accounts.selectDefault(id); if (alive) { if (accountsRev < 0) accounts.set(value); error.set(""); } }
    catch (reason) { fail(reason); }
  }
  async function refreshKey() {
    try { const value = await services.apiKey.refresh(); if (alive) apiKey.set(value); }
    catch (reason) { if (alive) apiError.set(String(reason)); }
  }
  async function saveKey() {
    if (apiPending() || !apiPaste().trim()) return;
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
      focusTheme(choices[(index + step + choices.length) % choices.length]);
    };
    const choose = () => { const id = themeFocus(); if (id) patch({ theme: id }); };
    return keymap.bind({ left: () => move(-1), up: () => move(-1), right: () => move(1), down: () => move(1), enter: choose, space: choose });
  });

  const muted = () => textStyle(theme, 10, true);
  // The shared button style has no border, so a focus color alone drew nothing, and an
  // unstyled Button does not dim itself when disabled. A transparent hairline takes the
  // focus color without moving the layout.
  const actionStyle = () => ({ ...buttonStyle(theme), borderWidth: 1, borderColor: "transparent" });
  const actionStates = () => ({ hoverStyle: { backgroundColor: theme().palette.divider }, focusStyle: { borderColor: theme().palette.focus }, disabledStyle: { opacity: 0.4 } });
  const button = (id: string, label: string, action: () => void, disabled = false, accessibilityLabel = label) => <Button testID={id} accessibilityLabel={accessibilityLabel} disabled={disabled} onPress={action} style={actionStyle()} hoverStyle={actionStates().hoverStyle} focusStyle={actionStates().focusStyle} disabledStyle={actionStates().disabledStyle}><Text style={textStyle(theme, 10)}>{label}</Text></Button>;
  // Notes wrap at the same measure as row descriptions.
  const note = (text: string | null | undefined, id?: string, tone?: "warning" | "error") => !!text && <Text testID={id} selectable={true} style={{ ...muted(), maxWidth: 560, color: tone === "error" ? theme().palette.danger : tone === "warning" ? theme().palette.gold : theme().palette.muted }}>{text}</Text>;
  const subheading = (text: string) => <Text role="header" style={{ ...textStyle(theme), fontWeight: "600", paddingTop: space(10, theme().scale) }}>{text}</Text>;
  const fieldStyle = (focused: boolean) => ({ color: theme().palette.text, backgroundColor: theme().palette.bg, padding: 8, borderWidth: 1, borderRadius: 6, borderColor: focused ? theme().palette.focus : theme().palette.divider });
  const saveError = () => error() || settings()?.error || "";
  function accountBody() {
    return <View style={{ gap: 8 }}>
      <ToggleRow id="codex-subscription-balancing" label="Balance Codex subscriptions" checked={settings()!.codexSubscriptionBalancing} onChange={(value) => patch({ codexSubscriptionBalancing: value })} theme={theme}
        description="New automatic Codex sessions use the saved account with the most quota left across its limits. Explicit accounts, project account choices and existing sessions stay fixed. Quota is checked before launch; missing, stale or exhausted accounts are excluded. If none qualify, choose an account explicitly or turn this off." />
      {note("Choose the account for new Codex sessions when subscription balancing is off, or the preferred account when balanced quotas tie. Running sessions keep their account. A project can override this in Project Settings.")}
      <View role="radiogroup" accessibilityLabel="Default Codex account" style={{ gap: 6 }}>
        {accounts()?.accounts.map((account) => {
          const selected = () => account.system ? !accounts()?.selectedId : accounts()?.selectedId === account.id;
          return <View testID={`codex-account-${account.id}`} role="radio" focusable={account.available} accessibilityLabel={account.label} accessibilityState={{ checked: selected(), selected: selected(), disabled: !account.available }}
            disabled={!account.available} onFocus={() => { if (alive) accountFocus.set(account.id); }} onBlur={() => { if (alive) accountFocus.set(null); }} onPress={() => { if (!alive) return; if (account.available) accountFocus.set(account.id); void selectAccount(account.system ? null : account.id, account.available); }} style={{ paddingHorizontal: space(6, theme().scale), paddingVertical: space(7, theme().scale), borderRadius: 7, gap: 3, borderWidth: 1, borderColor: "transparent", opacity: account.available ? 1 : 0.6, backgroundColor: selected() ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ borderColor: theme().palette.focus }}>
            <Text style={textStyle(theme)}>{account.label}</Text><Text style={muted()}>{account.detail}</Text>
            <Text style={{ ...muted(), color: !account.available ? theme().palette.gold : selected() ? theme().palette.cyan : theme().palette.muted }}>{!account.available ? "Unavailable" : selected() ? "Selected" : "Use account"}</Text>
          </View>;
        })}
      </View>
      {note(accounts()?.selectedUnavailable ? "Your selected account is unavailable. Refresh accounts or choose another before starting Codex." : null, "settings-account-unavailable", "warning")}
      {note(accounts()?.error, "settings-accounts-error", "error")}
      {note(accounts()?.fromCache ? "Showing saved accounts while Orca is unavailable." : null)}
      <View style={{ flexDirection: "row" }}>{button("refresh-codex-accounts", accounts()?.refreshing ? "Checking accounts…" : "Refresh accounts", () => void refreshAccounts(), !!accounts()?.refreshing)}</View>
    </View>;
  }
  function appearanceBody() {
    return <View style={{ gap: 8 }}>
      <View testID="settings-theme-grid"><ThemePicker catalog={paletteCatalog} selected={() => settings()!.theme} theme={theme} onChoose={(id) => { focusTheme(id); patch({ theme: id }); }}
        onNode={(id, node) => { themeNodes.set(id, node); scheduleInitialFocus(); }}
        onFocus={(id) => { if (alive) themeFocus.set(id); }} onBlur={(id) => { if (alive && themeFocus() === id) themeFocus.set(null); }} /></View>
      {settings()!.theme === "ghostty" && <ToggleRow id="terminal-colors" label="Use RiWork terminal colors" checked={settings()!.useRiworkColors} onChange={(value) => patch({ useRiworkColors: value })} theme={theme} />}
      {settings()!.theme === "native" && <ToggleRow id="terminal-colors" label="Terminals match the theme" checked={settings()!.nativeTerminalColors} onChange={(value) => patch({ nativeTerminalColors: value })} theme={theme} />}
      <ChoiceRow id="interface-font" label="Interface font" value={`interface-font-${settings()!.interfaceFont}`} choices={[{ id: "interface-font-system", label: "System (SF Pro)" }, { id: "interface-font-system_mono", label: "System + monospace accents" }]} onChange={(value) => patch({ interfaceFont: value === "interface-font-system" ? "system" : "system_mono" })} theme={theme} />
      <ToggleRow id="panel-tab-icons" label="Icons instead of labels" checked={settings()!.panelTabIcons} onChange={(value) => patch({ panelTabIcons: value })} theme={theme} />
      <ChoiceRow id="tab-close-behavior" label="When closing a tab" description="Nothing asks: the tab closes and its session keeps running. Offer to end shows a notice that can undo the close or end the session; Detach offers only Undo; Exit ends it at once. Workers always detach. This Mac only." value={`tab-close-${settings()!.tabCloseBehavior}`} choices={[{ id: "tab-close-ask", label: "Offer to end" }, { id: "tab-close-detach", label: "Detach" }, { id: "tab-close-exit", label: "Exit" }]} onChange={(value) => patch({ tabCloseBehavior: value === "tab-close-ask" ? "ask" : value === "tab-close-detach" ? "detach" : "exit" })} theme={theme} />
      <View testID="ui-text-size" focusable={true} accessibilityLabel="Interface text size" onFocus={() => { if (alive) textFocused.set(true); }} onBlur={() => { if (alive) textFocused.set(false); }} style={{ gap: 6, paddingVertical: 8 }}>
        <Text style={textStyle(theme)}>Interface text size</Text><View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Button testID="ui-text-smaller" accessibilityLabel="Smaller text" disabled={settings()!.uiTextMatchesTerminal || settings()!.uiTextSize <= 9} onPress={() => enqueue(() => services.settings.changeTextSize("smaller"))} style={actionStyle()} hoverStyle={actionStates().hoverStyle} focusStyle={actionStates().focusStyle} disabledStyle={actionStates().disabledStyle}><Text style={textStyle(theme)}>−</Text></Button>
          <Text testID="ui-text-points" style={{ ...textStyle(theme), opacity: settings()!.uiTextMatchesTerminal ? 0.5 : 1 }}>{`${settings()!.uiTextMatchesTerminal ? settings()!.text.points : settings()!.uiTextSize} pt`}</Text>
          <Button testID="ui-text-bigger" accessibilityLabel="Bigger text" disabled={settings()!.uiTextMatchesTerminal || settings()!.uiTextSize >= 24} onPress={() => enqueue(() => services.settings.changeTextSize("bigger"))} style={actionStyle()} hoverStyle={actionStates().hoverStyle} focusStyle={actionStates().focusStyle} disabledStyle={actionStates().disabledStyle}><Text style={textStyle(theme)}>+</Text></Button>
          {!settings()!.uiTextMatchesTerminal && settings()!.uiTextSize !== 11 && <Button testID="ui-text-reset" accessibilityLabel="Actual size" onPress={() => enqueue(() => services.settings.changeTextSize("reset"))} style={actionStyle()} hoverStyle={actionStates().hoverStyle} focusStyle={actionStates().focusStyle}><Text style={textStyle(theme)}>Reset</Text></Button>}
        </View>
        <ToggleRow id="ui-text-match-terminal" label="Match terminal" checked={settings()!.uiTextMatchesTerminal} onChange={(value) => patch({ uiTextMatchesTerminal: value })} theme={theme} />
        {note("Panels, tabs, menus and status bar. Matches Ghostty's font-size when enabled. ⌘+ bigger ⌘− smaller ⌘0 reset outside a terminal.")}
      </View>
    </View>;
  }
  // Native starts with neither `saved` nor `error` until the Keychain has been asked;
  // only an error means it could not say.
  function apiKeyState() {
    const state = apiKey(), palette = theme().palette;
    if (state.saved === true) return { text: "API key saved", color: palette.cyan };
    if (state.saved === false) return { text: "No API key saved", color: palette.muted };
    if (state.error) return { text: "The Keychain cannot say whether an API key is saved", color: palette.danger };
    return { text: "Checking the Keychain…", color: palette.muted };
  }
  function apiKeyRow() {
    return <View testID="claude-api-key" ref={(node) => { apiRow = node; scheduleReveal(); }} onLayout={scheduleReveal} style={{ paddingVertical: 8, gap: 6 }}>
      <Text style={textStyle(theme)}>Anthropic API key</Text>
      <Text testID="claude-api-key-state" style={{ ...muted(), color: apiKeyState().color }}>{apiKeyState().text}</Text>
      {!settings()!.claudeChatIgnoresApiKey && note("API key billing uses the key saved here. With no saved key, Claude uses a key your shell exports, or your Claude sign-in.")}
      <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 6 }}>
        <Input testID="claude-api-key-input" accessibilityLabel="Anthropic API key" ref={(node) => { apiInput = node; scheduleReveal(); }} masked={true} autoCapitalize="none" autoCorrect={false} placeholder="Paste an Anthropic API key" value={apiPaste()} onChange={(value) => { if (alive) apiPaste.set(value); }} onFocus={() => { if (alive) apiFocused.set(true); }} onBlur={() => { if (alive) apiFocused.set(false); }} onSubmit={() => { if (alive) void saveKey(); }} style={{ ...fieldStyle(apiFocused()), width: 220 }} />
        {button("claude-api-key-save", "Save", () => void saveKey(), apiPending() || !apiPaste().trim())}
        {apiKey().saved === true && button("claude-api-key-remove", "Remove", () => void removeKey(), apiPending())}
      </View>
      {note(apiError() || apiKey().error, "claude-api-key-error", "error")}
    </View>;
  }
  function agentBody() {
    const s = settings()!;
    return <View style={{ gap: 8 }}>
      <ToggleRow id="agent-inline-mode" label="Keep agent transcripts in scrollback (inline mode)" description="New Codex, Grok, and Claude sessions draw on the terminal's main screen, so the whole conversation stays in the terminal's scrollback. Off runs them full screen. Running sessions keep their mode until they restart." checked={s.agentInlineMode} onChange={(value) => patch({ agentInlineMode: value })} theme={theme} />
      <ToggleRow id="agent-terminals-unrestricted" label="Agent terminals run unrestricted" description="Codex, Claude, Grok and Muse terminals start without approval prompts. Off, they ask before running commands and editing files. Running sessions keep their mode." checked={s.agentTerminalsUnrestricted} onChange={(value) => patch({ agentTerminalsUnrestricted: value })} theme={theme} />
      <ToggleRow id="dictation-mic" label="Show microphone buttons for dictation" description="Covers chat message boxes on this Mac (or ⌃⌥D) and microphone buttons on a paired iPhone. Speech is recognized on the device you speak to. Off hides them all." checked={s.dictationMic} onChange={(value) => patch({ dictationMic: value })} theme={theme} />
      {s.dictationMic && <ChoiceRow id="push-to-talk" label="Push to talk" description="Hold the key alone in a chat (or hold its mic button) and talk; let go and the words are sent. A tap, or the key with another key, does nothing. ⌃⌥D and a click still dictate without sending." value={`push-to-talk-${s.pushToTalkKey}`} choices={[{ id: "push-to-talk-right_command", label: "Right ⌘" }, { id: "push-to-talk-right_option", label: "Right ⌥" }, { id: "push-to-talk-off", label: "Off" }]} onChange={(value) => patch({ pushToTalkKey: value.slice("push-to-talk-".length) as "right_command" | "right_option" | "off" })} theme={theme} />}
      {s.dictationMic && <SpeechModelPicker selected={() => settings()!.dictationModel} onChoose={(key) => patch({ dictationModel: key })} theme={theme} onError={fail} />}
      <ChoiceRow id="claude-billing" label="Claude chats bill" description="Subscription uses your Claude sign-in without ANTHROPIC_API_KEY. API key bills the key saved below, or uses Claude as installed when no key is saved. Each chat keeps its choice. Claude terminal tabs are not affected." value={s.claudeChatIgnoresApiKey ? "claude-billing-subscription" : "claude-billing-api-key"} choices={[{ id: "claude-billing-subscription", label: "Subscription" }, { id: "claude-billing-api-key", label: "API key" }]} onChange={(value) => patch({ claudeChatIgnoresApiKey: value === "claude-billing-subscription" })} theme={theme} />
      {apiKeyRow()}
      <View testID="new-chat-defaults" style={{ gap: 4, paddingVertical: 6 }}>
        <Text style={textStyle(theme)}>New chat defaults</Text>
        {note(s.newChatDefaults ? newChatSummary(s.newChatDefaults) : "None saved: a New chat tab starts with the provider of the chat in front, its default model and Supervised. Choose in a New chat tab and press Save as default.", "new-chat-defaults-summary")}
        {!!s.newChatDefaults && <View style={{ flexDirection: "row" }}>{button("new-chat-defaults-reset", "Reset", () => patch({ newChatDefaults: null }))}</View>}
      </View>
      <ToggleRow id="dashboard-haiku-summaries" label="Haiku dashboard summaries" description="Summarizes observed work for the dashboard using the saved Anthropic API key. Recent commands, file paths and messages from every provider's chats are sent to Anthropic. API usage is billed separately; decisions remain provider requests." checked={s.dashboardHaikuSummaries} onChange={(value) => patch({ dashboardHaikuSummaries: value })} theme={theme} />
      {subheading("Orchestrator")}
      <ChoiceRow id="orchestrator-mode" label="Orchestrator runs as" description="Automatic lets RiWork choose a native chat. Terminal uses Codex in a terminal tab. Chat uses the provider and model below." value={`orchestrator-mode-${s.orchestratorMode}`} choices={[{ id: "orchestrator-mode-automatic", label: "Automatic" }, { id: "orchestrator-mode-terminal", label: "Terminal" }, { id: "orchestrator-mode-chat", label: "Chat" }]} onChange={(value) => patch({ orchestratorMode: value === "orchestrator-mode-automatic" ? "automatic" : value === "orchestrator-mode-terminal" ? "terminal" : "chat" })} theme={theme} />
      {s.orchestratorMode === "automatic" && note("RiWork chooses a Muse chat on Wonder Full, or a Codex chat on GPT-6.1-Sol when Muse cannot start.", "orchestrator-automatic-note")}
      {s.orchestratorMode === "chat" && <>
        <ChoiceRow id="orchestrator-provider" label="Chat provider" description="Choosing another provider starts it on that provider's default model." value={`orchestrator-provider-${s.orchestratorChatProvider}`} choices={[{ id: "orchestrator-provider-codex", label: "Codex" }, { id: "orchestrator-provider-claude", label: "Claude" }, { id: "orchestrator-provider-muse", label: "Muse" }]} onChange={(value) => patch({ orchestratorChatProvider: value === "orchestrator-provider-codex" ? "codex" : value === "orchestrator-provider-claude" ? "claude" : "muse", orchestratorChatModel: null })} theme={theme} />
        <ChoiceRow id="orchestrator-model" label="Chat model" value={`orchestrator-model-${s.modelChoices.find((choice) => choice.model === s.orchestratorChatModel)?.id ?? "default"}`} choices={s.modelChoices.map((choice) => ({ id: `orchestrator-model-${choice.id}`, label: choice.label }))} onChange={(value) => patch({ orchestratorChatModel: s.modelChoices.find((choice) => `orchestrator-model-${choice.id}` === value)?.model ?? null })} theme={theme} />
        <View style={{ gap: 6 }}><Text style={textStyle(theme)}>Other model</Text><Input testID="orchestrator-other-model" accessibilityLabel="Other model ID" placeholder="Other model ID" value={modelInput()} onChange={(value) => { modelInput.set(value); if (modelError()) modelError.set(""); }} onFocus={() => { if (alive) modelFocused.set(true); }} onBlur={() => { if (alive) modelFocused.set(false); }} onSubmit={() => { const invalid = modelIdError(modelInput()); modelError.set(invalid ?? ""); if (!invalid) { patch({ orchestratorChatModel: modelInput().trim() }); modelInput.set(""); } }} style={{ ...fieldStyle(modelFocused()), width: 220 }} />{note(modelError(), "orchestrator-model-error", "error")}</View>
      </>}
      {note(s.orchestratorUnreadableNote, "orchestrator-unreadable-note", "warning")}
      {note("Applies to orchestrators created from now on, except in projects whose Project Settings choose otherwise. Existing orchestrators keep their mode, provider and model until closed and created again.")}
      {subheading("Delegation")}
      <ToggleRow id="delegation-spread-work" label="Spread work" description="Orchestrators fan independent tasks out to several worker shells at once instead of working through them one by one. Off, they keep to one worker at a time unless asked." checked={s.delegation.spreadWork} onChange={(value) => patch({ delegationPatch: { spreadWork: value } })} theme={theme} />
      <StepperRow id="delegation-parallel-shells" label="Parallel worker shells" value={s.delegation.parallelShells} minimum={1} maximum={10} onChange={(value) => patch({ delegationPatch: { parallelShells: value } })} theme={theme} />
      {(["design", "workhorse"] as const).map((work) => {
        const choices = work === "design" ? s.delegation.designChoices : s.delegation.workhorseChoices;
        const route = s.delegation[work], selected = choices.find((choice) => choice.route.provider === route.provider && choice.route.model === route.model);
        return <ChoiceRow id={`delegation-${work}`} label={work === "design" ? "Design work" : "Workhorse work"} value={`delegation-${work}-${selected?.id ?? "saved"}`} choices={choices.map((choice) => ({ id: `delegation-${work}-${choice.id}`, label: choice.label }))} onChange={(value) => { const route = choices.find((choice) => `delegation-${work}-${choice.id}` === value)?.route; if (route) patch({ delegationPatch: { [work]: route } }); }} theme={theme} />;
      })}
      {note(s.delegation.unreadableNote, "delegation-unreadable-note", "warning")}
    </View>;
  }
  // Items are grouped by the side they sit on, in their order there, so "earlier" and
  // "later" read against the list you see. Short labels on screen; the accessibility
  // labels name the item and the side.
  function statusBody() {
    return <View style={{ gap: 8 }}>
      <ToggleRow id="settings-status-enabled" label="Show status bar" checked={settings()!.statusBar.enabled} onChange={(value) => enqueue(() => services.settings.setEnabled(value))} theme={theme} />
      <ChoiceRow id="status-usage-mode" label="Account usage shows" value={`status-usage-${settings()!.statusBar.usageMode}`} choices={[{ id: "status-usage-current_chat", label: "Current chat" }, { id: "status-usage-all_accounts", label: "All accounts" }]} onChange={(value) => enqueue(() => services.settings.setUsageMode(value.slice(13)))} theme={theme} />
      {(["left", "right"] as const).map((side) => {
        const sameSide = settings()!.statusBar.items.filter((item) => (item.side === "left") === (side === "left")), other = side === "left" ? "right" : "left";
        return sameSide.length > 0 && <View testID={`settings-status-group-${side}`} style={{ gap: 0 }}>
          {subheading(side === "left" ? "Left side" : "Right side")}
          {sameSide.map((item, index) => <View testID={`settings-status-item-${item.kind}`} style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 8, paddingVertical: 2, borderBottomWidth: 1, borderColor: theme().palette.divider }}>
            <View style={{ flex: 1, minWidth: 180 }}>
              <ToggleRow id={`settings-status-visible-${item.kind}`} label={item.label} checked={item.enabled} onChange={(value) => enqueue(() => services.settings.setVisible(item.kind, value))} theme={theme} />
            </View>
            <View style={{ flexDirection: "row", gap: 5 }}>
              {button(`settings-status-side-${item.kind}`, `Move ${other}`, () => enqueue(() => services.settings.setSide(item.kind, other)), false, `Move ${item.label} to the ${other} side`)}
              {button(`settings-status-earlier-${item.kind}`, "Earlier", () => enqueue(() => services.settings.moveItem(item.kind, true)), index <= 0, `Move ${item.label} earlier on the ${side} side`)}
              {button(`settings-status-later-${item.kind}`, "Later", () => enqueue(() => services.settings.moveItem(item.kind, false)), index >= sameSide.length - 1, `Move ${item.label} later on the ${side} side`)}
            </View>
          </View>)}
        </View>;
      })}
      <View style={{ flexDirection: "row" }}>{button("settings-status-reset", "Reset defaults", () => enqueue(() => services.settings.resetStatusBar()))}</View>
    </View>;
  }
  function importBody() {
    const counts = () => receipt() ?? preview();
    return <View testID="orca-import" style={{ gap: 8 }}>
      <Text style={{ ...textStyle(theme), color: importPending() ? theme().palette.working : theme().palette.cyan }}>{importPending() ? "Working" : receipt() ? "Imported" : "One-time import"}</Text>
      {counts() && <><Text selectable={true} style={muted()}>{`ORCA CLI · ${counts()!.source}`}</Text><Text style={textStyle(theme)}>{`${counts()!.projectCount} projects${counts()!.folderCount > 0 ? ` · ${counts()!.folderCount} folders` : ""} · ${counts()!.worktreeCount} worktrees`}</Text></>}
      {!!preview()?.warnings.length && <Scroll style={{ maxHeight: 120 }}>{preview()!.warnings.map((warning) => note(warning, undefined, "warning"))}</Scroll>}
      {note(importPending(), "orca-import-pending")}{note(importError(), "orca-import-error", "error")}
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
  // The header sits in the sections' column, and a failed save shows there, in view,
  // instead of below the last section.
  return <SettingsFocus.Provider value={focus}><View testID="settings-panel" onLayout={(event) => { if (alive && event.size.width !== width()) width.set(event.size.width); }} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <View style={{ minHeight: space(46, theme().scale), paddingHorizontal: space(6, theme().scale), paddingVertical: space(6, theme().scale), justifyContent: "center", alignItems: "center" }}>
      <View style={{ width: "100%", maxWidth: settingsLayout(width()).maxWidth, paddingHorizontal: space(6, theme().scale), gap: space(1, theme().scale) }}>
        <Text role="header" style={{ ...textStyle(theme, 13), fontWeight: "600" }}>Settings</Text>
        {saveError() ? <Text testID="settings-save-error" selectable={true} style={{ ...muted(), color: theme().palette.danger }}>{saveError()}</Text> : <Text style={muted()}>All projects · Saved automatically</Text>}
      </View>
    </View>
    <Scroll testID="appearance-settings" ref={(node) => { page = node; scheduleReveal(); }} onScroll={(event) => { if (alive) scrollY.set(event.offset.y); }} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: space(12, theme().scale), alignItems: "center" }}>
      <View testID={`settings-layout-${settingsLayout(width()).kind}`} style={{ width: "100%", maxWidth: settingsLayout(width()).maxWidth, flexDirection: settingsLayout(width()).kind === "wide" ? "row" : "column", gap: settingsLayout(width()).gap }}>
        {sectionColumns(width()).map((column) => <View style={{ flex: 1, minWidth: 0, gap: 0 }}>{column.map((number) => {
          const section = SETTINGS_SECTIONS.find((section) => section.number === number)!;
          return <Section number={section.number} title={section.title} description={section.description} padding={settingsLayout(width()).card} theme={theme}>{body(number)}</Section>;
        })}</View>)}
      </View>
      <Text style={{ ...muted(), paddingVertical: 12 }}>Tab to move · Enter or Space to select · Arrows step the text size and worker shells</Text>
    </Scroll>
  </View></SettingsFocus.Provider>;
}
