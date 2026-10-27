import { Text, View, cleanup, effect, host, mount, signal } from "nsh";
import { Layouts, Settings, type LayoutSession, type SettingsPatch } from "riwork/native";
import { Workspace } from "../Workspace";
import { useAppearance } from "../appearance";
import type { HostParams } from "../model";

function Ready() {
  const params = host.params as unknown as HostParams & { diagnostics?: boolean };
  const ui = useAppearance(params);
  const session = signal<LayoutSession | null>(null);
  const notice = signal("");
  const modal = signal(false);
  let alive = true;
  if (params.diagnostics) effect(() => {
    const selected = ui.settings();
    if (selected) host.post("app.fixture.theme", { selected: selected.theme, native: ui.theme().native, text: ui.theme().palette.text });
  });
  cleanup(() => { alive = false; session()?.dispose(); });
  cleanup(host.on("app.fixture.notice", (event) => notice.set((event as { text: string }).text)));
  cleanup(host.on("app.fixture.modal", (event) => modal.set((event as { open: boolean }).open)));
  cleanup(host.on("app.fixture.settings", (event) => {
    void Settings.set(event as SettingsPatch).then(() => host.post("app.fixture.saved", {}));
  }));
  cleanup(host.on("app.fixture.status", (event) => {
    const edit = event as { action: string; kind: string; value: boolean; side: string };
    const operation = edit.action === "enabled" ? Settings.setEnabled(edit.value)
      : edit.action === "visible" ? Settings.setVisible(edit.kind, edit.value)
      : edit.action === "side" ? Settings.setSide(edit.kind, edit.side)
      : Settings.moveItem(edit.kind, edit.value);
    void operation.then(() => host.post("app.fixture.saved", {}));
  }));
  void Layouts.openSession({ windowId: params.windowId, projectId: params.projectId, restore: params.restore?.layout }).then((value) => {
    if (alive) session.set(value); else value.dispose();
  });
  return <View style={{ flex: 1 }}><Text testID="fixture-fonts">{`family:${ui.theme().family} mono:${ui.theme().monoFamily}`}</Text>{session() && ui.ready() && <Workspace session={session()!}
    title="RiWork · Project Alpha" projectLabel="Project Alpha" projectId={params.projectId ?? undefined}
    theme={ui.theme} settings={ui.settings} appearanceNotice={() => notice() || ui.notice()} modalOpen={modal} diagnostics={params.diagnostics} />}</View>;
}

function Fixture() {
  const params = host.params as unknown as { initial: SettingsPatch };
  const ready = signal(false);
  const error = signal("");
  void Settings.set(params.initial).then(() => ready.set(true)).catch((reason) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>{ready() ? <Ready /> : <Text>{error()}</Text>}</View>;
}
mount(Fixture);
