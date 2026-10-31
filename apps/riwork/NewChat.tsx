import { Scroll, Text, View, cleanup, effect, signal, type YNode } from "nsh";
import { Button, Input } from "nsh/base";
import { Chat, Settings, Store, type NewChatDefaultsDto, type NewChatRequest, type SettingsPatch } from "riwork/native";
import { space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";

type Provider = NewChatRequest["provider"];
type Mode = NonNullable<NewChatRequest["approvalMode"]>;
export interface NewChatModel {
  id: string;
  name: string;
  description?: string;
  efforts: string[];
  supportsFast: boolean;
  isDefault: boolean;
}
export interface NewChatCatalog {
  supported: NewChatModel[];
  configured: string[];
  accountLabel?: string | null;
  balanced?: boolean;
  error?: string | null;
  warning?: string | null;
}
export interface NewChatLocation { label: string; cwd: string; worktreeId?: string }
export interface NewChatProps {
  theme: () => InterfaceTheme;
  projectId: string;
  worktreeId?: string;
  cwd: string;
  locations?: NewChatLocation[];
  onCreated: (id: string, projectId: string) => void;
  onCancel: () => void;
  /** Whether its tab is on screen: the first time it is, the model search takes the focus. */
  visible?: () => boolean;
  /** The saved New chat defaults it starts with, over `provider` (Settings › New chat defaults). */
  defaults?: NewChatDefaultsDto | null;
  onError: (message: string) => void;
  provider?: Provider;
  unrestricted?: boolean;
  /** A catalog read supplies capabilities; saved chat names alone never do. */
  modelCatalog?: (provider: Provider, projectId: string, cwd: string) => Promise<NewChatCatalog>;
}

type NewChatDefaults = NonNullable<SettingsPatch["newChatDefaults"]>;
const PROVIDERS: Provider[] = ["codex", "claude", "muse"];
const providerName = (provider: Provider) => provider[0].toUpperCase() + provider.slice(1);
const MODES: { id: Mode; label: string; description: string }[] = [
  { id: "supervised", label: "Supervised", description: "Asks before commands and edits" },
  { id: "auto_edit", label: "Auto-edit", description: "Edits the workspace freely, asks for the rest" },
  { id: "full", label: "Full · unrestricted", description: "Never asks before acting" },
  { id: "plan", label: "Plan", description: "Plans first and changes nothing" },
];

/** A new chat's tab before it starts: its choices, and an optional first message. Only Start
 * creates the chat (the tab then becomes it); choosing providers and models reads metadata. */
export function NewChat(props: NewChatProps) {
  const projectId = props.projectId;
  // Keep the reviewed locations stable while the tab is open; confirm reads
  // the current store before creating so removed or moved folders cannot win.
  const locations = (props.locations?.length ? props.locations : [{ label: props.worktreeId ? "Selected worktree" : "Project root", cwd: props.cwd, worktreeId: props.worktreeId }]).map((location) => ({ ...location }));
  const locationIndex = signal(Math.max(0, locations.findIndex((location) => location.worktreeId === props.worktreeId && location.cwd === props.cwd)));
  const location = () => locations[locationIndex()];
  const saved = signal<NewChatDefaultsDto | null>(props.defaults ?? null);
  const savedProvider = PROVIDERS.find((item) => item === saved()?.provider), savedMode = MODES.find((item) => item.id === saved()?.mode)?.id;
  const provider = signal<Provider>(savedProvider ?? props.provider ?? "codex");
  const mode = signal<Mode>(props.unrestricted ? "full" : savedMode ?? "supervised");
  const model = signal(saved()?.model ?? ""), query = signal(""), title = signal(""), message = signal("");
  const effort = signal<string | null>(saved()?.effort ?? null), fast = signal(saved()?.fast ?? false);
  const catalogs = signal<Partial<Record<Provider, NewChatCatalog>>>({});
  const loading = signal(true), pending = signal(false), error = signal("");
  let alive = true, submitted = false;
  let search: YNode | undefined, focused = false;
  cleanup(() => { alive = false; });
  effect(() => { if (!focused && (props.visible?.() ?? true) && search) { focused = true; search.focus(); } });
  const theme = props.theme;
  const size = (value: number) => space(value, theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: theme().family, fontSize: value * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
  const field = () => ({ ...text(), minHeight: size(28), paddingHorizontal: size(8), paddingVertical: size(5), borderRadius: 6, backgroundColor: theme().palette.bg });
  const catalog = () => catalogs()[provider()];
  const selected = () => catalog()?.supported.find((item) => model().trim() ? item.id === model().trim() : item.isDefault);
  const modelChoices = () => {
    const current = catalog();
    const supported = current?.supported ?? [];
    return [...supported, ...(current?.configured ?? []).filter((id) => !supported.some((item) => item.id === id)).map((id) => ({ id, name: id, description: "Previously configured", efforts: [], supportsFast: false, isDefault: false }))]
      .filter((item) => `${item.name} ${item.id} ${item.description ?? ""}`.toLowerCase().includes(query().trim().toLowerCase()));
  };

  // The source bridge owns this catalog; the local generated declarations are
  // refreshed by the normal bundle build, not by opening this dialog.
  const catalogApi = Chat as typeof Chat & { models(provider: Provider, projectId?: string | null): Promise<NewChatCatalog> };
  const readCatalog = props.modelCatalog ?? ((kind: Provider, id: string) => catalogApi.models(kind, id));
  async function load() {
    try {
      await Promise.all(PROVIDERS.map(async (kind) => {
        try {
          const result = await readCatalog(kind, projectId, location().cwd);
          if (alive) catalogs.set({ ...catalogs(), [kind]: result });
        } catch (reason) {
          const saved = await Chat.list().catch(() => []);
          if (alive) catalogs.set({ ...catalogs(), [kind]: { supported: [], configured: [...new Set(saved.filter((chat) => chat.provider === kind && !!chat.model).map((chat) => chat.model!))], warning: `Model catalog unavailable: ${reason}` } });
        }
      }));
    } catch (reason) {
      if (alive) error.set(`Saved model choices could not be read: ${reason}`);
    } finally { if (alive) loading.set(false); }
  }
  void load();

  function chooseModel(value: string) { if (pending()) return; model.set(value); effort.set(null); fast.set(false); error.set(""); }
  function chooseProvider(value: Provider) {
    if (pending() || value === provider()) return;
    provider.set(value); query.set(""); chooseModel("");
  }
  function cancel() { if (pending() || submitted) return; submitted = true; props.onCancel(); }
  async function create() {
    if (!alive || pending() || submitted || loading()) return;
    const id = model().trim(), name = title().trim(), metadata = selected();
    if (id && ([...id].length > 100 || /[\x00-\x1f\x7f-\x9f]/.test(id))) { error.set("Use a model ID of at most 100 characters without control characters."); return; }
    if ([...name].length > 200 || /[\x00-\x1f\x7f-\x9f]/.test(name)) { error.set("Use a title of at most 200 characters without control characters."); return; }
    if (catalog()?.error) { error.set(catalog()!.error!); return; }
    pending.set(true); error.set("");
    const destination = location();
    const request: NewChatRequest = { provider: provider(), projectId, worktreeId: destination.worktreeId, cwd: destination.cwd, approvalMode: mode(),
      model: id || null, title: name || null, effort: effort() && metadata?.efforts.includes(effort()!) ? effort() : null,
      fast: fast() && metadata?.supportsFast === true };
    try {
      const current = await Store.snapshot();
      const project = current.projects.find((item) => item.id === projectId);
      const worktree = destination.worktreeId ? current.worktrees.find((item) => item.id === destination.worktreeId && item.projectId === projectId) : null;
      if (!project || (destination.worktreeId ? !worktree?.present || worktree.path !== destination.cwd : project.root !== destination.cwd)) {
        throw new Error("The chat's project or worktree changed. Open New chat again.");
      }
      const chat = await Chat.create(request);
      submitted = true;
      const first = message().trim();
      if (first) await sendFirst(chat.id, first);
      props.onCreated(chat.id, projectId);
    } catch (reason) {
      if (alive) { const message = String(reason).replace(/^Error: /, ""); error.set(message); props.onError(message); }
    } finally { if (alive) pending.set(false); }
  }
  const choices = (): NewChatDefaults => ({ provider: provider(), model: model().trim() || null, effort: effort(), fast: fast(), mode: mode() });
  const key = (value: NewChatDefaults | NewChatDefaultsDto | null) => value ? [value.provider, value.model ?? "", value.effort ?? "", value.fast, value.mode].join("|") : "";
  const savingDefaults = signal(false);
  async function saveDefaults() {
    if (savingDefaults() || pending()) return;
    savingDefaults.set(true); error.set("");
    try {
      const next = await Settings.set({ newChatDefaults: choices() });
      if (alive) saved.set(next.newChatDefaults ?? null);
    } catch (reason) {
      if (alive) error.set(`The defaults were not saved: ${String(reason).replace(/^Error: /, "")}`);
    } finally { if (alive) savingDefaults.set(false); }
  }
  /** The first message starts the chat's first turn; one that is not sent stays as its draft. */
  async function sendFirst(chatId: string, text: string) {
    const handle = Chat.open(chatId), editor = Chat.newEditor();
    try {
      const receipt = await handle.submit({ text, editor, generation: 1 });
      if (receipt.status !== "submitted") {
        handle.saveDraft(text, editor, 1);
        props.onError(`The first message was not sent${receipt.error ? `: ${receipt.error}` : ""}. It is in the chat's message box.`);
      }
    } catch (reason) {
      handle.saveDraft(text, editor, 1);
      props.onError(`The first message was not sent: ${String(reason).replace(/^Error: /, "")}. It is in the chat's message box.`);
    } finally { handle.close(); }
  }
  function choice(id: string, label: string, selected: boolean, onPress: () => void) {
    return <View testID={id} role="radio" accessibilityLabel={label} accessibilityState={{ checked: selected }} disabled={pending()} onPress={onPress}
      style={{ paddingHorizontal: size(9), paddingVertical: size(5), borderRadius: 6, backgroundColor: selected ? theme().palette.bg : "transparent" }}
      hoverStyle={{ backgroundColor: theme().palette.bg }}>
      <Text style={text(11, !selected)}>{label}</Text>
    </View>;
  }
  return <Scroll testID="new-chat-setup" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.bg }} contentStyle={{ alignItems: "center", padding: size(20) }}>
    <View style={{ width: "100%", maxWidth: size(600), gap: size(12) }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: size(8) }}><Icon name="chat" size={17 * theme().scale} color={theme().palette.muted} /><Text role="header" style={{ ...text(17), fontWeight: "600" }}>New chat</Text></View>
    <Text style={{ ...text(10, true), fontWeight: "600" }}>Location</Text>
    <View role="radiogroup" accessibilityLabel="Chat location" style={{ gap: 3 }}>
      {locations.map((item, index) => <Button testID={`new-chat-location-${item.worktreeId ?? "project"}`} selected={locationIndex() === index} disabled={pending()}
        accessibilityLabel={item.label} onPress={() => locationIndex.set(index)}
        style={{ paddingHorizontal: size(9), paddingVertical: size(6), borderRadius: 6, alignItems: "flex-start", backgroundColor: locationIndex() === index ? theme().palette.panelActive : "transparent" }}
        focusStyle={{ borderWidth: 1, borderColor: theme().palette.focus }}><Text style={text(11, locationIndex() !== index)}>{item.label}</Text></Button>)}
    </View>
    <Text selectable={true} style={{ ...text(10, true), fontFamily: theme().monoFamily }}>{location().cwd}</Text>
    <Text style={{ ...text(10, true), fontWeight: "600" }}>Permissions</Text>
    <View role="radiogroup" accessibilityLabel="Permissions" style={{ flexDirection: "row", flexWrap: "wrap", gap: 2, padding: 2, borderRadius: 8, backgroundColor: theme().palette.panelActive }}>
      {MODES.map((item) => choice(`new-chat-mode-${item.id}`, item.label, mode() === item.id, () => mode.set(item.id)))}
    </View>
    <Text style={text(10, true)}>{MODES.find((item) => item.id === mode())!.description}</Text>
    <View role="radiogroup" accessibilityLabel="Provider" style={{ flexDirection: "row", alignSelf: "flex-start", gap: 2, padding: 2, borderRadius: 8, backgroundColor: theme().palette.panelActive }}>
      {PROVIDERS.map((item) => choice(`new-chat-provider-${item}`, providerName(item), provider() === item, () => chooseProvider(item)))}
    </View>
    <Input testID="new-chat-model-search" ref={(node) => { search = node; }} accessibilityLabel="Search models" placeholder="Search models" value={query()} onChange={(value) => query.set(value)} disabled={pending()} style={field()} />
    <Scroll testID="new-chat-model-list" style={{ maxHeight: size(180), minHeight: size(32) }} contentStyle={{ gap: 2 }}>
      <View role="radiogroup" accessibilityLabel="Model" style={{ gap: 2 }}>
        {choice("new-chat-model-default", `${providerName(provider())} default`, !model().trim(), () => chooseModel(""))}
        {modelChoices().map((item) => <View testID={`new-chat-model-${item.id}`} role="radio" accessibilityLabel={item.name} accessibilityState={{ checked: model().trim() === item.id }} disabled={pending()} onPress={() => chooseModel(item.id)}
          style={{ paddingHorizontal: size(9), paddingVertical: size(6), gap: 2, borderRadius: 6, backgroundColor: model().trim() === item.id ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
          <Text numberOfLines={1} style={text()}>{`${item.name}${item.isDefault ? " · default" : ""}`}</Text>
          {item.description && <Text numberOfLines={2} style={text(10, true)}>{item.description}</Text>}
        </View>)}
      </View>
    </Scroll>
    <Input testID="new-chat-model-input" accessibilityLabel="Model ID" placeholder="Or type a model ID" value={model()} onChange={chooseModel} disabled={pending()} style={field()} />
    <Text style={text(10, true)}>{loading() ? "Reading saved model choices…" : catalog()?.warning || (catalog()?.supported.length ? `A typed ID is a ${providerName(provider())} model. Availability is checked when the chat starts.` : `Use ${providerName(provider())}'s default, a saved choice, or type a model ID.`)}</Text>
    {provider() === "codex" && catalog()?.accountLabel && <Text style={text(10, true)}>{`Codex account · ${catalog()?.balanced ? "balanced at start (now " : ""}${catalog()!.accountLabel}${catalog()?.balanced ? ")" : ""}`}</Text>}
    {!!selected()?.efforts.length && <View style={{ gap: 5 }}>
      <Text style={{ ...text(10, true), fontWeight: "600" }}>Reasoning</Text>
      <View role="radiogroup" accessibilityLabel="Reasoning effort" style={{ flexDirection: "row", flexWrap: "wrap", gap: 2, padding: 2, borderRadius: 8, backgroundColor: theme().palette.panelActive }}>
        {choice("new-chat-effort-default", "Default effort", effort() === null, () => effort.set(null))}
        {selected()!.efforts.map((value) => choice(`new-chat-effort-${value}`, value, effort() === value, () => effort.set(value)))}
      </View>
    </View>}
    {selected()?.supportsFast && <View testID="new-chat-fast" role="checkbox" accessibilityLabel="Fast mode" accessibilityState={{ checked: fast() }} disabled={pending()} onPress={() => fast.set(!fast())} style={{ flexDirection: "row", alignItems: "center", gap: 7 }}><Icon name={fast() ? "check" : "add"} size={12 * theme().scale} color={theme().palette.muted} /><Text style={text()}>Fast mode</Text></View>}
    <Input testID="new-chat-title" accessibilityLabel="Chat title" placeholder="Title (optional)" value={title()} onChange={(value) => title.set(value)} disabled={pending()} style={field()} />
    <Text style={{ ...text(10, true), fontWeight: "600" }}>First message</Text>
    <Input testID="new-chat-message" accessibilityLabel="First message" placeholder="Optional: what the chat starts with" multiline={true} autoCorrect={false}
      value={message()} onChange={(value) => message.set(value)} disabled={pending()} style={{ ...field(), minHeight: size(64), maxHeight: size(200) }} />
    {(!!error() || !!catalog()?.error) && <Text role="alert" style={{ ...text(11), color: theme().palette.gold }}>{error() || catalog()?.error}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", alignItems: "center", gap: size(8) }}>
      <Button testID="new-chat-save-defaults" accessibilityLabel="Save these choices as the defaults of new chats" disabled={pending() || savingDefaults() || key(saved()) === key(choices())}
        onPress={() => void saveDefaults()} style={{ marginRight: "auto", paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 6 }}>
        <Text style={text(11, true)}>{key(saved()) === key(choices()) ? "These are your defaults" : savingDefaults() ? "Saving…" : "Save as default"}</Text>
      </Button>
      <Button testID="new-chat-cancel" disabled={pending()} onPress={cancel} style={{ paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 6 }}><Text style={text()}>Cancel</Text></Button>
      <Button testID="new-chat-confirm" disabled={pending() || loading() || !!catalog()?.error} onPress={() => void create()} style={{ paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 6, backgroundColor: theme().palette.panelActive }}><Text style={{ ...text(), fontWeight: "600" }}>{pending() ? "Starting…" : message().trim() ? "Start chat" : "Create chat"}</Text></Button>
    </View>
    </View>
  </Scroll>;
}
