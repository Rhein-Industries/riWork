import { Text, View, cleanup, computed, effect, signal } from "nsh";
import { ApiKey, Settings, type ChatHandle, type ChatPanel } from "riwork/native";
import type { InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";

type Billing = "subscription" | "api_key";
interface BillingProjection {
  selected?: Billing | null;
  reported?: Billing | null;
  source?: string | null;
  limits: { id: string; resetsAt?: number | null }[];
}
type Panel = ChatPanel & { billing?: BillingProjection | null };
interface BillingResult { action: "settings" | "changed" | "restarted"; message: string }
interface BillingHandle { billingAction(action: string, noticeId?: string): Promise<BillingResult> }
interface BillingSummary { label: string; short: string; hint: string; on: boolean; mismatch: boolean; settings: boolean }

/** One controller per chat: the toolbar and limit banners share key-presence and
 * settings subscriptions. Neither the controller nor its bridge receives a key. */
export function createBillingControl(props: {
  panel: () => Panel;
  handle: ChatHandle;
  visible?: () => boolean;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
  onOpenSettings: () => void;
}) {
  const keySaved = signal<boolean | null>(null), defaultBilling = signal<Billing>("subscription"), pending = signal(false);
  const clock = signal(Date.now());
  let alive = true, keyRev = -1, settingsRev = -1;
  const available = computed(() => props.panel().provider === "claude");
  const subscriptions = [
    Settings.subscribe((event) => {
      if (!alive || event.rev <= settingsRev) return;
      settingsRev = event.rev;
      defaultBilling.set(event.data.claudeChatIgnoresApiKey ? "subscription" : "api_key");
    }),
    ApiKey.subscribe((event) => {
      if (!alive || event.rev <= keyRev) return;
      keyRev = event.rev; keySaved.set(event.data.saved ?? null);
    }),
  ];
  void Settings.get().then((settings) => {
    if (alive && settingsRev < 0) defaultBilling.set(settings.claudeChatIgnoresApiKey ? "subscription" : "api_key");
  }).catch((error) => { if (alive && available()) props.onError(String(error)); });
  effect(() => {
    if (!available() || !(props.visible?.() ?? true)) return;
    const before = keyRev;
    void ApiKey.refresh().then((value) => {
      if (alive && keyRev === before) keySaved.set(value.saved ?? null);
    }).catch((error) => { if (alive) props.onError(String(error)); });
  });
  effect(() => {
    const now = clock();
    const next = props.panel().billing?.limits.map((limit) => (limit.resetsAt ?? 0) * 1000).filter((at) => at > now).sort((a, b) => a - b)[0];
    if (next === undefined) return;
    const timer = setTimeout(() => { if (alive) clock.set(Date.now()); }, Math.min(2_147_483_647, Math.max(1, next - Date.now())));
    return () => clearTimeout(timer);
  });
  cleanup(() => { alive = false; for (const subscription of subscriptions) subscription.close(); });

  const summary = computed<BillingSummary>(() => {
    const projection = props.panel().billing;
    const selected = projection?.selected ?? defaultBilling();
    const shown = projection?.reported ?? selected;
    const source = projection?.source;
    const saved = keySaved() === true;
    let hint: string, mismatch = false, settings = false;
    if (shown === "api_key") {
      if (selected === "subscription") {
        mismatch = true;
        hint = `Set to Subscription, but Claude still bills an API key (${source ?? "of its own"}) from Claude's own settings. Switch to restart on your subscription.`;
      } else {
        hint = source ? `This chat bills an Anthropic API key (${source}).` : saved
          ? "This chat bills the Anthropic API key saved in Settings."
          : "No API key is saved in Settings, so Claude uses its installed credentials: a key exported by your shell, or your Claude sign-in.";
        hint += " Switch to your Claude subscription; Claude restarts on the same conversation.";
      }
    } else if (selected === "api_key" && saved) {
      mismatch = true;
      hint = "Set to API key, but this Claude started before a key was saved and bills your subscription. Switch to stop it; the next message starts on the key.";
    } else if (saved) {
      hint = "This chat bills your Claude subscription. Switch to your Anthropic API key; Claude restarts on the same conversation.";
    } else {
      settings = true;
      hint = "This chat bills your Claude subscription. Switch to enter an Anthropic API key in Settings and bill it instead.";
    }
    const short = shown === "api_key" ? "API key" : "Subscription";
    return { label: `Billing: ${short}`, short, hint, on: shown === "api_key", mismatch, settings };
  });
  const disabled = () => {
    const panel = props.panel();
    return pending() || !available() || panel.running || panel.state === "starting" || panel.state === "waiting" || panel.link !== "live";
  };
  const offer = (noticeId: string): "use_api_key" | "enter_api_key" | undefined => {
    const limit = props.panel().billing?.limits.find((item) => item.id === noticeId);
    if (!available() || !limit || (limit.resetsAt != null && limit.resetsAt * 1000 <= Math.max(clock(), Date.now()))) return undefined;
    return keySaved() === true ? "use_api_key" : "enter_api_key";
  };
  function settings() { if (alive) props.onOpenSettings(); }
  async function run(action: "toggle" | "use_api_key", noticeId?: string) {
    if (!alive || disabled()) return;
    pending.set(true);
    try {
      const result = await (props.handle as unknown as BillingHandle).billingAction(action, noticeId);
      if (!alive) return;
      if (result.action === "settings") settings();
      else if (result.message) props.onNotice(result.message);
    } catch (error) { if (alive) props.onError(String(error)); }
    finally { if (alive) pending.set(false); }
  }
  function toggle() {
    if (disabled()) return;
    if (summary().settings) settings(); else void run("toggle");
  }
  function useApiKey(noticeId: string) {
    const action = offer(noticeId);
    if (action === "enter_api_key") settings();
    else if (action === "use_api_key") void run("use_api_key", noticeId);
  }
  return { available, summary, disabled, pending, offer, toggle, useApiKey, settings };
}

export type BillingControl = ReturnType<typeof createBillingControl>;

export function BillingControls(props: { control: BillingControl; theme: () => InterfaceTheme; narrow?: boolean }) {
  const { control, theme } = props;
  const color = () => control.summary().mismatch ? theme().palette.gold : control.summary().on ? theme().palette.working : theme().palette.muted;
  return <View style={{ display: control.available() ? "flex" : "none" }}>
    <View testID="chat-billing" role="switch" focusable={!control.disabled()} tooltip={control.summary().hint}
      accessibilityLabel={control.summary().label} accessibilityHint={control.summary().hint}
      accessibilityState={{ checked: control.summary().on, disabled: control.disabled() }} onPress={control.toggle}
      style={{ height: 27 * theme().scale, paddingHorizontal: 7 * theme().scale, flexDirection: "row", gap: 4 * theme().scale, alignItems: "center", justifyContent: "center", borderRadius: 5,
        opacity: control.disabled() ? 0.4 : 1, backgroundColor: "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Icon name={control.summary().on ? "key" : "person"} size={11 * theme().scale} color={color()} />
      {(!props.narrow || control.pending()) && <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: 10 * theme().scale, color: color() }}>
        {control.pending() ? "Switching…" : control.summary().short}
      </Text>}
    </View>
  </View>;
}

export function BillingNoticeAction(props: { control: BillingControl; noticeId: string; theme: () => InterfaceTheme }) {
  const { control, theme } = props;
  const enter = () => control.offer(props.noticeId) === "enter_api_key";
  const disabled = () => !enter() && control.disabled();
  const label = () => control.pending() ? "Switching…" : enter() ? "Enter API key" : "Use API key";
  const hint = () => enter() ? "Open Settings at the Anthropic API key field." : "Switch this conversation to your saved Anthropic API key. A text-only message rejected by this limit is sent again only if nothing has been sent since.";
  return <View style={{ display: control.offer(props.noticeId) ? "flex" : "none", flexShrink: 0 }}>
    <View role="button" focusable={!disabled()} tooltip={hint()} accessibilityLabel={label()} accessibilityHint={hint()}
      accessibilityState={{ disabled: disabled() }} onPress={() => { if (!disabled()) control.useApiKey(props.noticeId); }}
      style={{ minHeight: 23 * theme().scale, paddingHorizontal: 7 * theme().scale, justifyContent: "center", borderRadius: 4,
        opacity: disabled() ? 0.4 : 1, backgroundColor: theme().palette.panelActive }}>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: 10 * theme().scale, color: theme().palette.text }}>{label()}</Text>
    </View>
  </View>;
}
