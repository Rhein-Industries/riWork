import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { Files, Store } from "riwork/native";
import { FilesPanel, MemoEditor, PreviewPanel, createFilesController } from "../Files";

function Body(props: { root: string; projectId: string }) {
  const root = signal<string | null>(props.root), label = signal("Isolated Files fixture");
  const active = signal<"files" | "preview" | null>("files");
  const edits = signal<string[]>([]), opens = signal<string[]>([]), requests = signal(0);
  const memo = signal(""), memoOpen = signal(false), exact = signal<string | null>(null);
  const controller = createFilesController({ root, label, active,
    previewRequested: () => requests.set(requests() + 1),
    edit: async (entry) => { await Files.validateEdit(props.root, entry.path, entry.identity!); edits.set([...edits(), entry.path]); },
    open: (entry) => { opens.set([...opens(), entry.path]); },
  });
  const subscription = Store.subscribe((event) => memo.set(event.data.projectMemos[props.projectId]?.text ?? ""));
  cleanup(() => subscription.close());
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", {
    root: root(), selected: controller.selected()?.relativePath ?? null,
    rows: controller.rows().map((row) => row.entry.relativePath),
    preview: controller.preview(), previewPath: controller.previewEntry()?.relativePath ?? null,
    edits: edits(), opens: opens(), requests: requests(), query: controller.query(), memo: memo(), notice: controller.notice(), exact: exact(),
  }));
  host.on("app.fixture.passive", (event) => {
    const path = (event as { path: string }).path;
    const entry = [...controller.listings().values()].flatMap((listing) => listing.entries).find((entry) => entry.relativePath === path);
    if (entry) controller.select(entry, false);
  });
  return <View style={{ flex: 1 }}>
    <View style={{ flexDirection: "row", gap: 8 }}>
      <Button testID="fixture-root-none" onPress={() => root.set(null)}><Text>No root</Text></Button>
      <Button testID="fixture-root-restore" onPress={() => root.set(props.root)}><Text>Restore root</Text></Button>
      <Button testID="fixture-relabel" onPress={() => label.set("Relabelled fixture")}><Text>Relabel</Text></Button>
      <Button testID="fixture-active-files" onPress={() => active.set("files")}><Text>Files keys</Text></Button>
      <Button testID="fixture-active-preview" onPress={() => active.set("preview")}><Text>Preview keys</Text></Button>
      <Button testID="fixture-memo-toggle" onPress={() => memoOpen.set(!memoOpen())}><Text>Toggle memo</Text></Button>
      <Button testID="fixture-read-exact" onPress={() => { const entry = controller.selected(); if (entry?.identity) void Files.readText(props.root, entry.path, entry.identity).then((text) => exact.set(text)); }}><Text>Read exact bytes</Text></Button>
    </View>
    {memoOpen() ? <MemoEditor projectId={() => props.projectId} /> : <View style={{ flex: 1, minHeight: 0, flexDirection: "row" }}>
      <View style={{ width: 360, minHeight: 0 }}><FilesPanel controller={controller} surface="fixture" /></View>
      <PreviewPanel controller={controller} />
    </View>}
    <Text testID="fixture-saved-memo">{memo()}</Text>
  </View>;
}

function Fixture() {
  const params = host.params as { key: string };
  const ready = signal<{ root: string; projectId: string } | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  void Files.fixtureDirectory(params.key).then(async (root) => {
    const project = await Store.createProject({ root, name: `Files fixture ${params.key}`, initGit: false });
    if (alive) ready.set({ root, projectId: project.id });
  }).catch((reason) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Body root={ready()!.root} projectId={ready()!.projectId} /> : <Text testID="fixture-error">{error() || "Preparing isolated files fixture…"}</Text>}</View>;
}
mount(Fixture);
