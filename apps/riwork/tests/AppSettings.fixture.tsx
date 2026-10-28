import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Accounts, ApiKey, Settings, type AccountsDto, type SettingsDto, type SettingsPatch } from "riwork/native";
import { AppSettings, type AppSettingsControl, type AppSettingsServices, type ImportReceipt } from "../settings/AppSettings";
import { ChoiceRow } from "../settings/SettingsControls";
import { DEFAULT_THEME } from "../appearance";

/** The acceptance runner builds native test-support: fake accounts, in-memory
 * API key storage, and an Orca import preview that never spawns the CLI. */
function Fixture() {
  const params = host.params as { revealImmediately?: boolean; keyboardFixture?: boolean; importScenario?: string };
  const settings = signal<SettingsDto | null>(null), accounts = signal<AccountsDto | null>(null);
  const keySaved = signal<boolean | null>(null), notice = signal("");
  const fixtureChoice = signal("fixture-choice-a"), importAttempts = signal(0);
  let previewSequence = 0, imported: ImportReceipt | null = null;
  const services: AppSettingsServices | undefined = params.importScenario ? {
    settings: Settings, accounts: Accounts, apiKey: ApiKey,
    orcaImport: {
      preview: async () => ({ id: `fixture-preview-${++previewSequence}`, source: "/isolated/fixture/orca", projectCount: 2, folderCount: 0, worktreeCount: 3, alreadyImported: imported, recordable: true, warnings: ["Fixture warning: projects will be unfiled."] }),
      run: async () => {
        const attempt = importAttempts() + 1; importAttempts.set(attempt);
        if (params.importScenario === "stale" && attempt === 1) throw new Error("Orca or RiWork changed since this preview. Refresh the preview before importing.");
        imported = { source: "/isolated/fixture/orca", projectCount: 2, folderCount: 0, worktreeCount: 3, completedAt: 1 };
        return imported;
      },
    },
  } : undefined;
  let control: AppSettingsControl | undefined;
  let original: SettingsDto | null = null, originalAccount: string | null | undefined;
  const subscriptions = [Settings.subscribe((event) => { original ??= event.data; settings.set(event.data); }), Accounts.subscribe((event) => { if (originalAccount === undefined) originalAccount = event.data.selectedId ?? null; accounts.set(event.data); }), ApiKey.subscribe((event) => keySaved.set(event.data.saved ?? null))];
  cleanup(() => { for (const subscription of subscriptions) subscription.close(); });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", { settings: settings(), accounts: accounts(), keySaved: keySaved(), notice: notice(), fixtureChoice: fixtureChoice(), importAttempts: importAttempts() }));
  host.on("app.fixture.reveal-key", () => control?.revealApiKey());
  host.on("app.fixture.restore", () => {
    void (async () => {
      const before = original;
      if (before) await Settings.set({
        theme: before.theme, useRiworkColors: before.useRiworkColors, nativeTerminalColors: before.nativeTerminalColors,
        openPreviewOnSelect: before.openPreviewOnSelect, rememberWindowSize: before.rememberWindowSize, panelTabIcons: before.panelTabIcons,
        tabCloseBehavior: before.tabCloseBehavior, agentInlineMode: before.agentInlineMode, agentTerminalsUnrestricted: before.agentTerminalsUnrestricted,
        dictationMic: before.dictationMic, claudeChatIgnoresApiKey: before.claudeChatIgnoresApiKey,
        codexSubscriptionBalancing: before.codexSubscriptionBalancing, dashboardHaikuSummaries: before.dashboardHaikuSummaries,
        orchestratorMode: before.orchestratorMode, orchestratorChatProvider: before.orchestratorChatProvider, orchestratorChatModel: before.orchestratorChatModel ?? null,
        delegationPatch: { spreadWork: before.delegation.spreadWork, parallelShells: before.delegation.parallelShells, design: before.delegation.design, workhorse: before.delegation.workhorse },
        uiTextSize: before.uiTextSize, uiTextMatchesTerminal: before.uiTextMatchesTerminal, interfaceFont: before.interfaceFont, statusBar: before.statusBar,
      } as SettingsPatch);
      await Accounts.selectDefault(originalAccount ?? null);
      host.post("app.fixture.restored", {});
    })().catch((reason) => host.post("app.fixture.restored", { error: String(reason) }));
  });
  return <View style={{ flex: 1 }}>
    <AppSettings settings={settings} services={services} theme={() => ({ ...DEFAULT_THEME, native: settings()?.theme === "native", scale: settings()?.text.scale ?? 1 })}
      onControl={(value) => { control = value; if (params.revealImmediately) value.revealApiKey(); }} onNotice={(value) => notice.set(value)}
      computerUse={<Text>Computer use fixture slot</Text>} remote={<Text>Remote fixture slot</Text>} />
    {params.keyboardFixture && <ChoiceRow id="fixture-choice" label="Fixture keyboard choice" value={fixtureChoice()} choices={[{ id: "fixture-choice-a", label: "A" }, { id: "fixture-choice-disabled", label: "Unavailable", disabled: true }, { id: "fixture-choice-c", label: "C" }]} onChange={(value) => fixtureChoice.set(value)} />}
    <Text testID="settings-fixture-notice">{notice()}</Text>
  </View>;
}
mount(Fixture);
