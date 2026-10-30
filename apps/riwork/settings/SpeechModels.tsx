import { Text, cleanup, signal } from "nsh";
import { SpeechModels } from "riwork/native";
import { Button, View } from "../SettingsFocus";
import { Icon } from "../Icon";
import { space } from "../appearance";
import { buttonStyle, textStyle, type Theme } from "./SettingsControls";

/** One dictation choice as the native catalog reports it, with what is on this Mac. */
export interface SpeechModel {
  key: string; label: string; detail: string; family: string; bytes: number; cachedBytes: number; downloadable: boolean;
  /** builtin, ready, setup (on this Mac, runtime not set up), missing, unavailable, installing or downloading. */
  status: string; error?: string | null;
}
export interface SpeechCatalog { models: SpeechModel[]; runtime: { ready: boolean; uv: boolean }; cache?: string | null }
export interface SpeechModelService {
  snapshot(): Promise<SpeechCatalog>;
  subscribe(listener: (catalog: SpeechCatalog) => void): { close(): void };
  download(key: string): Promise<SpeechCatalog>;
  cancel(key: string): Promise<SpeechCatalog>;
}
// The bridge types its JSON values as unknown; the shapes are speech.rs's snapshot.
const nativeService = SpeechModels as unknown as SpeechModelService;

export function size(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

/** What a row says about the model's files, under its description. */
export function speechModelState(model: SpeechModel): string {
  switch (model.status) {
    case "builtin": return "Built in";
    case "ready": return `On this Mac · ${size(model.bytes)}`;
    case "setup": return `On this Mac · ${size(model.bytes)} · needs a one-time setup`;
    case "installing": return "Setting up the speech runtime…";
    case "downloading": {
      const percent = model.bytes ? Math.floor((model.cachedBytes / model.bytes) * 100) : 0;
      return `Downloading ${size(model.cachedBytes)} of ${size(model.bytes)} · ${percent}%`;
    }
    case "unavailable": return "No longer downloadable";
    default: return `${size(model.bytes)} download`;
  }
}

/** Dictation's speech model: Apple's on-device speech, or a local MLX model RiWork downloads.
 * Choosing and downloading are separate, so one click never starts a download of many gigabytes. */
export function SpeechModelPicker(props: { selected: () => string; onChoose: (key: string) => void; theme: Theme; service?: SpeechModelService; onError?: (reason: unknown) => void }) {
  const theme = props.theme, service = props.service ?? nativeService;
  const catalog = signal<SpeechCatalog | null>(null);
  let alive = true;
  const subscription = service.subscribe((next) => { if (alive) catalog.set(next); });
  void service.snapshot().then((next) => { if (alive && !catalog()) catalog.set(next); }).catch((reason) => props.onError?.(reason));
  cleanup(() => { alive = false; subscription.close(); });
  // The subscription is the one source of state: a reply can be older than what it delivered.
  const act = (action: Promise<SpeechCatalog>) => void action.catch((reason) => props.onError?.(reason));
  const muted = (size = 10) => textStyle(theme, size, true);

  function row(model: SpeechModel) {
    const chosen = () => props.selected() === model.key;
    const busy = model.status === "downloading" || model.status === "installing";
    const action = model.status === "missing" ? "Download" : model.status === "setup" ? "Set up" : busy ? "Cancel" : null;
    const selectable = model.status !== "unavailable";
    return <View testID={`speech-model-${model.key}`} style={{ flexDirection: "row", alignItems: "center", gap: space(8, theme().scale), paddingVertical: space(4, theme().scale) }}>
      <Button testID={`speech-model-choose-${model.key}`} role="radio" accessibilityLabel={`${model.label}, ${speechModelState(model)}`}
        selected={chosen()} disabled={!selectable}
        onPress={() => props.onChoose(model.key)}
        style={{ flex: 1, minWidth: 0, flexDirection: "row", alignItems: "flex-start", gap: space(8, theme().scale), padding: space(7, theme().scale), borderRadius: 8,
          borderWidth: 1, borderColor: chosen() ? theme().palette.focus : theme().palette.divider, backgroundColor: chosen() ? theme().palette.panelActive : "transparent" }}
        hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ borderColor: theme().palette.focus }}>
        <View style={{ paddingTop: 1 }}><Icon name={chosen() ? "task-done" : "task-todo"} size={13 * theme().scale} color={chosen() ? theme().palette.text : theme().palette.muted} /></View>
        <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
          <Text style={{ ...textStyle(theme), fontWeight: chosen() ? "600" : "400", opacity: selectable ? 1 : 0.6 }}>{model.label}</Text>
          <Text style={muted()}>{model.detail}</Text>
          <Text testID={`speech-model-state-${model.key}`} style={{ ...muted(), color: busy ? theme().palette.working : model.status === "ready" || model.status === "builtin" ? theme().palette.cyan : theme().palette.muted }}>{speechModelState(model)}</Text>
          {!!model.error && <Text testID={`speech-model-error-${model.key}`} selectable={true} style={{ ...muted(), color: theme().palette.gold }}>{model.error}</Text>}
          {chosen() && (model.status === "missing" || model.status === "setup") && <Text style={{ ...muted(), color: theme().palette.gold }}>Dictation uses this model once it is on this Mac.</Text>}
        </View>
      </Button>
      {action && <Button testID={`speech-model-${action === "Cancel" ? "cancel" : "download"}-${model.key}`} accessibilityLabel={`${action} ${model.label}`}
        disabled={action !== "Cancel" && !catalog()?.runtime.uv && !catalog()?.runtime.ready}
        onPress={() => act(action === "Cancel" ? service.cancel(model.key) : service.download(model.key))} style={buttonStyle(theme)}
        focusStyle={{ borderColor: theme().palette.focus }} hoverStyle={{ backgroundColor: theme().palette.divider }}>
        <Text style={textStyle(theme)}>{action}</Text>
      </Button>}
    </View>;
  }

  return <View testID="speech-models" style={{ gap: 4, paddingVertical: space(7, theme().scale) }}>
    <Text style={textStyle(theme)}>Dictation model</Text>
    <Text style={{ ...muted(), maxWidth: 560 }}>Every model runs on this Mac; audio never leaves it. Local models download from Hugging Face into its shared cache, so a model another app such as riTranscribe downloaded is used as it is. The first one also sets up a Python runtime (about 450 MB) in RiWork's data folder.</Text>
    {catalog()?.models.map(row)}
    {!catalog() && <Text style={muted()}>Checking which models are on this Mac…</Text>}
    {catalog() && !catalog()!.runtime.uv && !catalog()!.runtime.ready && <Text testID="speech-models-uv" style={{ ...muted(), color: theme().palette.gold }}>Local models need uv to set up their runtime. Install it with `brew install uv`, then come back here.</Text>}
    {!!catalog()?.cache && <Text selectable={true} style={muted(9)}>{`Model cache: ${catalog()!.cache}`}</Text>}
  </View>;
}
