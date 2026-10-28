import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Files, Store } from "riwork/native";
import { MemoEditor } from "../Files";

function Body(props: { projectId: string }) {
  type Receipt = Awaited<ReturnType<typeof Store.setProjectMemo>>;
  type Request = {
    text: string; revision: Parameters<typeof Store.setProjectMemo>[2]; phase: "requested" | "writing" | "committed" | "delivered" | "failed";
    result?: Receipt; resolve: (result: Receipt) => void; reject: (reason: unknown) => void;
  };
  const requests: Request[] = [];
  const error = signal("");
  const saveMemo: typeof Store.setProjectMemo = (projectId, text, revision) => new Promise((resolve, reject) => {
    if (projectId !== props.projectId) { reject(new Error("Unexpected memo project")); return; }
    requests.push({ text, revision, phase: "requested", resolve, reject });
  });
  cleanup(host.on("app.fixture.commit", (event) => {
    const request = requests[(event as { index: number }).index];
    if (!request || request.phase !== "requested") return;
    request.phase = "writing";
    // Persist first, but keep its receipt pending until the test delivers it.
    void Store.setProjectMemo(props.projectId, request.text, request.revision).then((result) => {
      request.result = result; request.phase = "committed";
    }).catch((reason) => { request.phase = "failed"; error.set(String(reason)); request.reject(reason); });
  }));
  cleanup(host.on("app.fixture.receipt", (event) => {
    const request = requests[(event as { index: number }).index];
    if (!request?.result || request.phase !== "committed") return;
    request.phase = "delivered"; request.resolve(request.result);
  }));
  cleanup(host.on("app.fixture.inspect", () => {
    // Read through the real core store, not the component's last subscription.
    void Store.snapshot().then((state) => host.post("app.fixture.state", {
      memo: state.projectMemos[props.projectId]?.text ?? "",
      requests: requests.map(({ text, phase }) => ({ text, phase })), error: error(),
    }));
  }));
  return <View style={{ flex: 1 }}><MemoEditor projectId={() => props.projectId} saveMemo={saveMemo} />
    <Text testID="receipt-fixture-error">{error()}</Text></View>;
}

function Fixture() {
  const key = (host.params as { key: string }).key;
  const ready = signal<string | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  void (async () => {
    const root = await Files.fixtureDirectory(key);
    const project = await Store.createProject({ root, name: `Memo receipt ${key}`, initGit: false });
    await Store.setProjectMemo(project.id, "A");
    if (alive) ready.set(project.id);
  })().catch((reason) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Body projectId={ready()!} /> : <Text>{error() || "Preparing isolated memo fixture…"}</Text>}</View>;
}
mount(Fixture);
