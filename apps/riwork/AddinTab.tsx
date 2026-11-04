import { Scroll, Text, View, cleanup, nativeComponent, signal, type YNode } from "nsh";
import { Addins } from "riwork/native";
import type { InterfaceTheme } from "./appearance";

/** What the Add-ins panel and a tab know of one (Addins.list). */
export interface AddinInfo {
  key: string; id?: string; name?: string; version?: string; description?: string; scope: "project" | "personal";
  dir: string; installed?: boolean; trusted: boolean; problem?: string | null;
  views?: { id: string; title: string; icon?: string }[];
  permissions?: { id: string; text: string }[]; network?: string[];
  workers?: { id: string; description: string }[]; tools?: { name: string; description: string }[];
  commands?: { id: string; title: string }[];
  settings?: { key: string; label: string; type: "string" | "number" | "boolean"; description: string; value: unknown }[];
}

/** What an add-in asks its tab to do: Somtus does it, where the person sees it. */
export type AddinAction =
  | { type: "command"; id: string; args?: Record<string, unknown> }
  | { type: "shell"; command: string; cwd: string; worktree?: string | null; title?: string }
  | { type: "file"; path: string }
  | { type: "view"; addin: string; view: string; title?: string | null; args: unknown }
  | { type: "notify"; title: string; body: string }
  | { type: "status"; text: string | null; tone: "normal" | "warning" | "critical" };

interface AddinState { status: "loading" | "untrusted" | "failed" | "build-failed" | "running"; error?: string; key?: string }

const AddinSurface = nativeComponent<
  { addin: string; view: string; project: string; args: unknown; palette: unknown; context: unknown },
  { state: AddinState; action: AddinAction }
>("riwork.addin", { props: ["addin", "view", "project", "args", "palette", "context"], events: ["state", "action"] });

const list = (projectId: string) => (Addins.list(projectId) as Promise<unknown>) as Promise<AddinInfo[]>;

/** An add-in's view in a tab: its own engine (riwork.addin), and what Somtus says around it. */
export function AddinTab(props: {
  addin: { key: string; view: string; args: unknown }; projectId: () => string; theme: () => InterfaceTheme;
  visible: () => boolean; onAction: (key: string, action: AddinAction) => void; onFocus?: () => void;
  /** Native code projects only the granted workspace fields into the add-in. */
  workspace?: () => unknown;
}) {
  const theme = props.theme;
  const state = signal<AddinState>({ status: "loading" });
  const info = signal<AddinInfo | null>(null);
  const busy = signal(false);
  let surface: YNode | undefined;
  let alive = true;
  cleanup(() => { alive = false; });
  const refresh = () => void list(props.projectId()).then((addins) => {
    if (alive) info.set(addins.find((addin) => addin.key === props.addin.key || props.addin.key === `id:${addin.id}`) ?? null);
  }).catch(() => {});
  refresh();
  const changes = Addins.subscribe(() => refresh());
  cleanup(() => changes.close());

  const text = (size = 12, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  const button = (label: string, action: () => void, primary = false, testID?: string) =>
    <View testID={testID} role="button" accessibilityLabel={label} onPress={() => { if (!busy()) action(); }}
      style={{ paddingHorizontal: 12, paddingVertical: 6, borderRadius: 6, opacity: busy() ? 0.5 : 1,
        backgroundColor: primary ? theme().palette.cyan : theme().palette.panelActive }}
      hoverStyle={{ backgroundColor: primary ? theme().palette.cyan : theme().palette.divider }}>
      <Text style={text(12, primary ? theme().palette.bg : theme().palette.text)}>{label}</Text>
    </View>;
  async function trust() {
    busy.set(true);
    try {
      await Addins.trust(props.projectId(), props.addin.key);
      surface?.command("retry");
    } catch (error) { state.set({ status: "failed", error: String(error) }); }
    finally { if (alive) busy.set(false); }
  }
  const name = () => info()?.name ?? props.addin.key;
  const commands = () => state().status === "running" ? info()?.commands ?? [] : [];

  return <View testID={`addin-tab:${props.addin.key}`} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.bg }}>
    {commands().length > 0 && <View style={{ flexDirection: "row", gap: 6, paddingHorizontal: 10, paddingVertical: 6, borderBottomWidth: 1, borderBottomColor: theme().palette.divider }}>
      {commands().map((command) => <View role="button" testID={`addin-command:${command.id}`} accessibilityLabel={command.title}
        onPress={() => surface?.command("command", { id: command.id })}
        style={{ paddingHorizontal: 10, paddingVertical: 4, borderRadius: 5, backgroundColor: theme().palette.panelActive }}
        hoverStyle={{ backgroundColor: theme().palette.divider }}>
        <Text style={text(11)}>{command.title}</Text>
      </View>)}
    </View>}
    {state().status === "build-failed" && <View testID="addin-build-failed" style={{ paddingHorizontal: 12, paddingVertical: 6, backgroundColor: theme().palette.panel, borderBottomWidth: 1, borderBottomColor: theme().palette.gold }}>
      <Text numberOfLines={3} selectable={true} style={text(11, theme().palette.gold)}>{`The last change did not build; this is the version before it. ${state().error ?? ""}`}</Text>
    </View>}
    <View style={{ flex: 1, minHeight: 0 }}>
      <AddinSurface ref={(node) => { surface = node; }} addin={props.addin.key} view={props.addin.view} project={props.projectId()}
        args={props.addin.args ?? null} palette={theme().palette} context={props.workspace?.() ?? null}
        onState={(next) => { state.set(next); if (next.status === "running" || next.status === "untrusted") refresh(); }}
        onAction={(action) => props.onAction(props.addin.key, action)}
        style={{ flex: 1, opacity: state().status === "running" || state().status === "build-failed" ? 1 : 0 }} />
      {state().status !== "running" && state().status !== "build-failed" && <View style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0, alignItems: "center", justifyContent: "center", padding: 24, backgroundColor: theme().palette.bg }}>
        <Scroll style={{ maxWidth: 560, width: "100%" }} contentStyle={{ gap: 12 }}>
          {state().status === "loading" && <Text testID="addin-loading" style={text(13, theme().palette.muted)}>{`Building ${name()}…`}</Text>}
          {state().status === "failed" && <>
            <Text style={text(14)}>{`${name()} could not start`}</Text>
            <Text testID="addin-error" selectable={true} style={{ ...text(11, theme().palette.gold), fontFamily: theme().monoFamily }}>{state().error ?? ""}</Text>
            <View style={{ flexDirection: "row", gap: 8 }}>
              {button("Retry", () => surface?.command("retry"), true, "addin-retry")}
              {button("Show folder", () => void Addins.reveal(props.projectId(), props.addin.key))}
            </View>
          </>}
          {state().status === "untrusted" && <View testID="addin-trust" style={{ gap: 10 }}>
            <Text style={text(15)}>{`Open ${name()}?`}</Text>
            <Text style={text(11, theme().palette.muted)}>{info()?.scope === "project"
              ? "This add-in comes with the project. Trust it only if you trust the project's code."
              : info()?.installed ? "This add-in was installed from a signed package." : "This is one of your own add-ins."}</Text>
            {(info()?.permissions ?? []).length > 0 && <View style={{ gap: 4 }}>
              <Text style={text(12)}>It may:</Text>
              {info()!.permissions!.map((permission) => <Text style={text(12, theme().palette.muted)}>{`· ${permission.text}`}</Text>)}
              {(info()?.network ?? []).map((origin) => <Text style={text(11, theme().palette.muted)}>{`   ${origin}`}</Text>)}
            </View>}
            {(info()?.workers ?? []).length > 0 && <Text style={text(12, theme().palette.gold)}>{`It runs native code of its own: ${info()!.workers!.map((worker) => worker.id).join(", ")}.`}</Text>}
            {(info()?.tools ?? []).length > 0 && <Text style={text(12, theme().palette.muted)}>{`Agents can use its tools: ${info()!.tools!.map((tool) => tool.name).join(", ")}.`}</Text>}
            <Text style={text(11, theme().palette.muted)}>You are asked again when what it may do, or its native code, changes.</Text>
            <View style={{ flexDirection: "row", gap: 8 }}>
              {button("Trust and open", () => void trust(), true, "addin-trust-open")}
              {button("Show folder", () => void Addins.reveal(props.projectId(), props.addin.key))}
            </View>
          </View>}
        </Scroll>
      </View>}
    </View>
  </View>;
}
