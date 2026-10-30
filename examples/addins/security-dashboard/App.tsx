import { List, Text, View, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { info, notify, onCommand, project, setStatus, settings, startWorker, tasks, type Worker } from "./addin-api";

interface Finding { file: string; line: number; kind: string }

function App() {
  const palette = (info() as unknown as { palette?: Record<string, string> }).palette ?? {};
  const ink = palette.text ?? "#222", muted = palette.muted ?? "#777", gold = palette.gold ?? "#b60";
  const findings = signal<Finding[]>([]), files = signal(0), scanning = signal(false), error = signal(""), filed = signal<Set<string>>(new Set());
  let worker: Worker | undefined, root = "";

  async function scan() {
    if (scanning()) return;
    scanning.set(true); error.set("");
    try {
      root ||= (await project.get()).root;
      if (!worker) {
        worker = await startWorker("scanner");
        worker.on("progress", (data) => files.set((data as { files: number }).files));
      }
      const skip = String((await settings()).skip ?? "").split(",").map((name) => name.trim()).filter(Boolean);
      const result = await worker.request<{ findings: Finding[]; files: number }>("scan", { root, skip });
      findings.set(result.findings); files.set(result.files);
      await setStatus(result.findings.length ? `${result.findings.length} secrets found` : null, result.findings.length ? "critical" : "normal");
      if (result.findings.length) await notify("Secrets in the project", `${result.findings.length} findings in ${result.files} files`);
    } catch (reason) { error.set(String(reason)); }
    finally { scanning.set(false); }
  }
  async function file(finding: Finding) {
    await tasks.add(`Remove the ${finding.kind} in ${finding.file}:${finding.line}`, "Found by the Security dashboard. Rotate the secret, then remove it from the file and its history.");
    filed.set(new Set([...filed(), `${finding.file}:${finding.line}`]));
  }
  onCommand((id) => { if (id === "rescan") void scan(); });
  void scan();

  return <View style={{ flex: 1, padding: 18, gap: 10 }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
      <Text style={{ fontSize: 18, fontWeight: "600", color: ink, flex: 1 }}>Security</Text>
      <Text style={{ color: muted }}>{scanning() ? `Scanning… ${files()} files` : `${files()} files scanned`}</Text>
    </View>
    {error() !== "" && <Text selectable={true} style={{ color: gold }}>{error()}</Text>}
    {!scanning() && findings().length === 0 && error() === "" && <Text style={{ color: muted }}>No secrets found.</Text>}
    <List each={findings()} keyExtractor={(finding) => `${finding.file}:${finding.line}`} estimatedItemSize={44} style={{ flex: 1 }}>
      {(finding) => <View style={{ flexDirection: "row", alignItems: "center", gap: 10, paddingVertical: 6 }}>
        <View style={{ flex: 1, gap: 2 }}>
          <Text style={{ color: ink }}>{finding().kind}</Text>
          <Text selectable={true} style={{ color: muted, fontSize: 11 }}>{`${finding().file}:${finding().line}`}</Text>
        </View>
        <Button onPress={() => void file(finding())} disabled={filed().has(`${finding().file}:${finding().line}`)}
          style={{ paddingHorizontal: 10, paddingVertical: 4, borderRadius: 6, backgroundColor: "#8883" }}>
          <Text style={{ color: ink, fontSize: 12 }}>{filed().has(`${finding().file}:${finding().line}`) ? "Task added" : "Add task"}</Text>
        </Button>
      </View>}
    </List>
  </View>;
}
mount(App);
