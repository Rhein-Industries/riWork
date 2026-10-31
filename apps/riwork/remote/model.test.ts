import { expect, test } from "nsh/test";
import {
  EMPTY_REMOTE, checkPairingLink, filteredFolders, linkPresentation, maskedPairingLink,
  projectConnectionNote, projectNameError, remoteErrorNotice, remoteLocalOnly, remotePanelUnavailable,
  remoteProjectKey, remoteStrip, remoteTabTitle, remoteTitle, removalConfirmation, samePrompt, shellOpenRefusal, singleFlight, splitRemoteTitle,
  type RemoteLists, type RemoteShell, type RemoteSnapshot,
} from "./model";

const shell: RemoteShell = { id: "same-id", projectId: "project", worktreeId: null, orchestrator: false, chat: false, harness: "codex", alive: true, cwd: "/remote/project", display: "Codex same-id" };
const project: RemoteLists = { key: "remote:fake-mac:project", host: "fake-mac", project: "project", hostLabel: "Fixture Mac", projectName: "Remote project", link: "online", unpaired: false,
  worktrees: [], worktreesLoading: false, worktreesError: null, tasks: [], tasksLoading: false, tasksError: null, shells: [shell], shellsLoading: false, shellsError: null, creating: false, failure: null };
const data: RemoteSnapshot = { ...EMPTY_REMOTE, hosts: [{ id: "fake-mac", label: "Fixture Mac", link: "online", status: "Online" }], projects: [project],
  folders: [{ host: "fake-mac", label: "Fixture Mac", link: "offline", count: 2, creating: false, failure: null, note: "Offline (or access revoked)", projects: [
    { key: project.key, id: "project", name: "Remote project", dimmed: true, stats: "1 trees · 0/1 tasks · 1 live" },
    { key: "remote:fake-mac:other", id: "other", name: "Other", dimmed: true, stats: "– trees · –/– tasks · – live" },
  ] }] };

test("P6B-REMOTE-MODEL-01 remote namespace, title and unavailable actions never fall back to local", () => {
  expect(remoteProjectKey(project.key)).toEqual({ host: "fake-mac", project: "project" });
  for (const key of ["project", "remote:fake-mac", "remote::project", "remote:fake-mac:"]) expect(remoteProjectKey(key)).toBe(null);
  expect(remoteProjectKey("remote:fake-mac:project:extra")).toEqual({ host: "fake-mac", project: "project:extra" });
  expect(remoteLocalOnly(data, "remote:bad host:project")).toContain("Not available");
  expect(remoteTitle(data, project.key)).toBe("Remote project (on Fixture Mac)");
  expect(remoteTitle(data, "local-project")).toBe(null);
  expect(remoteLocalOnly(data, project.key)).toBe("Not available for a project on Fixture Mac");
  expect(remoteLocalOnly(data, project.key, "handoff")).toBe("Hand off works on the projects of this Mac.");
  expect(remoteLocalOnly(data, "local-project")).toBe(null);
  for (const panel of ["files", "preview", "memo", "project_settings", "schedules", "usage"]) expect(remotePanelUnavailable(panel)).toBe(true);
  for (const panel of ["projects", "settings", "worktrees", "tasks", "shells"]) expect(remotePanelUnavailable(panel)).toBe(false);
});

test("P6B-REMOTE-MODEL-02 last-known projects stay searchable and dimmed; source snapshot is untouched", () => {
  expect(filteredFolders(data, "remote project")[0].projects.map((row) => row.id)).toEqual(["project"]);
  expect(filteredFolders(data, "fixture mac")[0].projects.length).toBe(2);
  expect(filteredFolders(data, "no match").length).toBe(0);
  expect(filteredFolders(data, "project")[0].projects[0].dimmed).toBe(true);
  expect(data.folders[0].projects.length).toBe(2);
  expect(projectConnectionNote({ ...project, unpaired: true })).toBe("This Mac is no longer paired. Add it again in Settings.");
  expect(projectConnectionNote({ ...project, link: "offline" })).toBe("Offline (or access revoked)");
  expect(projectConnectionNote({ ...project, link: null })).toBe("Connecting to Fixture Mac…");
  expect(projectConnectionNote(project)).toBe(null);
});

test("P6B-REMOTE-MODEL-03 host labels and terminal strip preserve offline/ended distinctions", () => {
  expect(linkPresentation("online")).toEqual({ dot: "●", tone: "cyan", text: "Online" });
  expect(linkPresentation("offline")).toEqual({ dot: "○", tone: "muted", text: "Offline (or access revoked)" });
  expect(remoteStrip("Fixture Mac", null, false)).toBe(null);
  expect(remoteStrip("Fixture Mac", "online", false)).toBe(null);
  expect(remoteStrip("Fixture Mac", "connecting", false)?.text).toBe("RECONNECTING · Fixture Mac");
  expect(remoteStrip("Fixture Mac", "offline", false)?.text).toBe("RECONNECTING · Fixture Mac · offline, or access was revoked");
  expect(remoteStrip("Fixture Mac", "offline", true)).toEqual({ text: "Ended on Fixture Mac", tone: "magenta", reconnect: true });
});

test("P6B-REMOTE-MODEL-04 chat/dead shell refusals preserve host attribution", () => {
  expect(shellOpenRefusal(shell, "Fixture Mac")).toBe(null);
  expect(shellOpenRefusal({ ...shell, alive: false }, "Fixture Mac")).toBe("Codex same-id has ended on Fixture Mac");
  expect(shellOpenRefusal({ ...shell, chat: true }, "Fixture Mac")).toBe("Codex same-id runs as a chat on Fixture Mac; chats run on that Mac. Open it there, or in the iOS app.");
});

test("P6B-REMOTE-MODEL-05 pairing links validate without reflecting secrets and generated masks expose only 16 characters", () => {
  expect(checkPairingLink("")).toBe("Paste the pairing link from the other Mac.");
  for (const value of ["https://invalid.example/", "riwork://pair?fake token", "riwork://pair?fake\nsecond"]) expect(checkPairingLink(value)).toBe("That is not a RiWork pairing link.");
  const fake = "riwork://pair?DISPOSABLE_NOT_A_CREDENTIAL";
  expect(checkPairingLink(`  ${fake}  `)).toBe(null);
  expect(maskedPairingLink(fake)).toBe(`${fake.slice(0, 16)}…  (${fake.length} characters)`);
  expect(maskedPairingLink(fake).includes("CREDENTIAL")).toBe(false);
});

test("P6B-REMOTE-MODEL-06 remote names match engine character/UTF8 and path restrictions", () => {
  for (const value of ["", ".hidden", "-option", "../escape", "a/b", "a\\b", "a\nline", "a\u2028line", "a\u2029line", "a".repeat(101), "漢".repeat(86)]) expect(projectNameError(value) !== null).toBe(true);
  expect(projectNameError("  A project ü  ")).toBe(null);
  expect(projectNameError("漢".repeat(85))).toBe(null);
  expect(projectNameError("a".repeat(100))).toBe(null);
});

test("P6B-REMOTE-MODEL-07 remove confirmation resets on prompt/host loss and ignores pending activation", () => {
  const confirm = removalConfirmation();
  expect(confirm.press("fake-mac", false)).toBe(false);
  expect(confirm.selected()).toBe("fake-mac");
  expect(confirm.press("fake-mac", true)).toBe(false);
  expect(confirm.press("fake-mac", false)).toBe(true);
  expect(confirm.selected()).toBe(null);
  confirm.press("fake-mac", false); confirm.reset();
  expect(confirm.press("fake-mac", false)).toBe(false);
  confirm.reconcile([]); expect(confirm.selected()).toBe(null);
});

test("P6B-REMOTE-MODEL-08 delayed and failed mutations are single-flight and never automatically retried", async () => {
  const gate = singleFlight(); let release: (() => void) | undefined, calls = 0;
  const first = gate.run(async () => { calls++; await new Promise<void>((resolve) => { release = resolve; }); return "first"; });
  expect(gate.pending()).toBe(true);
  expect(await gate.run(async () => { calls++; return "duplicate"; })).toEqual({ accepted: false });
  release!(); expect(await first).toEqual({ accepted: true, value: "first" });
  try { await gate.run(async () => { calls++; throw new Error("fixture timeout; outcome unknown"); }); } catch {}
  expect(calls).toBe(2); expect(gate.pending()).toBe(false);
});

test("a missing riwork-remote is one short actionable line; the build recipe stays as detail", () => {
  const missing = "Missing standalone remote binary at /a/riwork-remote or /a/RiWork.app/Contents/MacOS/riwork-remote. Run `riwork update --source /path/to/riWork --no-reload` to build and package it.";
  expect(remoteErrorNotice(missing)).toEqual({ summary: "Other Macs need the riwork-remote helper, which is not installed. Build it with riwork update.", detail: missing });
  expect(remoteErrorNotice("Relay unreachable. Retrying in 10 s")).toEqual({ summary: "Relay unreachable.", detail: "Relay unreachable. Retrying in 10 s" });
  expect(remoteErrorNotice("Relay unreachable")).toEqual({ summary: "Relay unreachable", detail: null });
});

test("2CB-REMOTE-TITLE-01 tab titles come from the native tree and split back into the host mark", () => {
  const key = "remote:fake-mac:0123456789abcdef";
  expect(remoteTabTitle(data, key)).toBe("⇄ Fixture Mac · 01234567");
  expect(remoteTabTitle({ ...data, hosts: [], folders: [] }, key)).toBe("⇄ fake-mac · 01234567");
  const titled = { ...data, titles: [{ key, title: "⇄ Fixture Mac · orchestrator (codex) · 01234567" }] };
  expect(remoteTabTitle(titled, key)).toBe("⇄ Fixture Mac · orchestrator (codex) · 01234567");
  expect(splitRemoteTitle(remoteTabTitle(titled, key))).toEqual(["⇄ Fixture Mac · ", "orchestrator (codex) · 01234567"]);
  expect(splitRemoteTitle("Shell 1")).toBe(null);
  expect(splitRemoteTitle("⇄ no separator")).toBe(null);
});

test("2CB-REMOTE-MODEL-09 a form's own opener folds it; another opener, or another Mac's, switches", () => {
  expect(samePrompt(null, { kind: "add" })).toBe(false);
  expect(samePrompt({ kind: "add" }, { kind: "add" })).toBe(true);
  expect(samePrompt({ kind: "add" }, { kind: "pair" })).toBe(false);
  expect(samePrompt({ kind: "project", host: "a", label: "A" }, { kind: "project", host: "a", label: "Renamed" })).toBe(true);
  expect(samePrompt({ kind: "project", host: "a", label: "A" }, { kind: "project", host: "b", label: "A" })).toBe(false);
});
