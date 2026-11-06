/** Disposable review data; no live hosts, providers, credentials, or session processes. */
import { View, Text, effect, host, mount, signal } from "nsh";
import { Store } from "riwork/native";
import { AddinsPanel, type AddinsPanelApi } from "../AddinsPanel";
import { DEFAULT_THEME } from "../appearance";
import { FilesPanel, PreviewPanel, createFilesController } from "../Files";
import { AppSettings } from "../settings/AppSettings";
import { privateFixtureDirectory, mountInPrivateHome } from "./private-home";

function AddinsFixture() {
  const opened = signal("");
  const rows = [
    { key: "overview", name: "Project overview", description: "Project health, active work and the next decisions in one place.", scope: "project", version: "0.1.0", trusted: false, dir: "/fixture/addins/overview", views: [{ id: "main", title: "Overview" }] },
    { key: "release", name: "Release checklist", description: "Track review, verification and release readiness.", scope: "project", version: "1.0.0", trusted: true, dir: "/fixture/addins/release", views: [{ id: "main", title: "Checklist" }] },
    { key: "notes", name: "Daily notes", description: "Your own notes, available in every project.", scope: "personal", trusted: true, dir: "/fixture/addins/notes", views: [{ id: "main", title: "Notes" }] },
  ];
  const api = { list: async () => rows, subscribe: () => ({ close() {} }), create: async () => ({ key: "created" }), trust: async () => {}, revoke: async () => {}, reveal: async () => {}, setSetting: async () => {} } as unknown as AddinsPanelApi;
  return <View style={{ flex: 1 }}><AddinsPanel api={api} projectId={() => "fixture"} theme={() => DEFAULT_THEME} visible={() => true} open={(key) => opened.set(key)} notice={() => {}} /><Text testID="review-opened">{opened()}</Text></View>;
}
function FileBody(props: { root: string; preview: boolean }) {
  const c = createFilesController({ root: () => props.root, label: () => "main · Somtus", previewRequested: () => {}, edit: () => {}, open: () => {}, reveal: () => {} });
  let seeded = false;
  effect(() => {
    const root = c.listings().get(props.root);
    if (!root || seeded) return;
    seeded = true;
    const entries = [
      { name: "apps", relativePath: "apps", path: `${props.root}/apps`, kind: "directory" as const },
      { name: "assets", relativePath: "assets", path: `${props.root}/assets`, kind: "directory" as const },
      { name: "crates", relativePath: "crates", path: `${props.root}/crates`, kind: "directory" as const },
      { name: "docs", relativePath: "docs", path: `${props.root}/docs`, kind: "directory" as const },
      { name: "Cargo.toml", relativePath: "Cargo.toml", path: `${props.root}/Cargo.toml`, kind: "file" as const, gitStatus: "M" },
      { name: "README.md", relativePath: "README.md", path: `${props.root}/README.md`, kind: "file" as const },
      { name: "shared", relativePath: "shared", path: `${props.root}/shared`, kind: "symlink" as const },
    ];
    c.listings.set(new Map([[props.root, { ...root, entries }]]));
    c.selected.set(entries[5]); c.previewEntry.set(entries[5]);
    c.preview.set({ kind: "text", markdown: true, truncated: false, text: "# Somtus\n\nA workspace for projects, agents, and the work they share.\n\n## Your workspace\n\nKeep the project tree beside a conversation. Browse a worktree and open files without losing your place.\n\n## Working together\n\n- Projects keep repositories and folders together.\n- Sessions brings chats and terminals into one list.\n- Tasks follows work from an idea to a finished change.\n- Automations runs prompts on your schedule.\n\n## Next up\n\nReview the active worktree, run the project checks, and share progress with the project orchestrator." });
  });
  return props.preview ? <PreviewPanel controller={c} /> : <FilesPanel controller={c} surface="review" />;
}
function Fixture() {
  const panel = (host.params as { panel?: string }).panel;
  const root = signal("");
  if (panel === "files" || panel === "preview") void privateFixtureDirectory(`panel-review-${panel}`).then((path) => root.set(path));
  if (panel === "addins") return <AddinsFixture />;
  if (panel === "settings") return <AppSettings />;
  return <View style={{ flex: 1 }}>{root() && <FileBody root={root()} preview={panel === "preview"} />}</View>;
}
mountInPrivateHome(Fixture);
