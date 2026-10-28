import { expect, test } from "nsh/test";
import { AutomationModel, defaults, editorFor, localIso, optionsLabel, projectScope, quickTime, repeatMinutes, selectScope, selectTarget, singleLine, status, targetsFor, validate, visibleRows, type ScheduleApi, type ScheduleRow } from "./model";

const now = 1_900_000_000;
function row(extra: Partial<ScheduleRow> = {}): ScheduleRow {
  return { key: { id: "schedule", revision: 4, scope: projectScope("project"), targetId: "pinned" }, title: "Review", prompt: "Inert prompt", destination: "new_chat", destinationLabel: "New Claude project chat", options: { ...defaults(), provider: "claude" }, accountLabel: "Claude system login", firstAt: now + 600, firstIso: localIso(now + 600), everyMinutes: null, paused: false, reviewRequired: false, nextRun: now + 600, nextDisplay: "2030-03-17 18:00:00 +01:00", lastRun: null, ...extra };
}
function failure(work: () => unknown): string { try { work(); return ""; } catch (reason) { return String(reason); } }

test("P6B-SCHED-01 defaults and each quick pick are measured from the click", () => {
  const editor = editorFor("project", now);
  expect(editor.fresh).toBe(true); expect(editor.options).toEqual(defaults());
  expect(editor.scope).toEqual(projectScope("project")); expect(editor.repeat).toBe("once"); expect(editor.minutes).toBe("60");
  expect(Date.parse(editor.at) / 1000).toBe(now + 600);
  expect(Date.parse(quickTime(editor, 1800, now + 50).at) / 1000).toBe(now + 1850);
  expect(editorFor("project", now, row({ nextRun: now - 1 })).at).toBe(localIso(now + 600));
  expect(editorFor("project", now, row({ everyMinutes: 17 })).repeat).toBe("minutes");
});

test("P6B-SCHED-02 custom repeat zero and invalid dates cannot become a one-time run", () => {
  const editor = { ...editorFor("project", now), title: "Review", prompt: "Prompt" };
  for (const minutes of ["0", "4", "5.5", "-10", "525601"]) expect(failure(() => repeatMinutes({ ...editor, repeat: "minutes", minutes })).includes("Repeat interval")).toBe(true);
  expect(failure(() => repeatMinutes({ ...editor, repeat: "minutes", minutes: "" })).includes("Enter repeat minutes")).toBe(true);
  expect(repeatMinutes({ ...editor, repeat: "minutes", minutes: "5" })).toBe(5);
  expect(repeatMinutes({ ...editor, repeat: "once", minutes: "0" })).toBe(null);
  expect(failure(() => validate({ ...editor, at: "2030-03-17T10:00:00" }, now)).includes("explicit UTC offset")).toBe(true);
  expect(failure(() => validate({ ...editor, at: localIso(now) }, now)).includes("future")).toBe(true);
  expect(singleLine("a\r\nb\nc\u2028d")).toBe("a b c d");
});

test("P6B-SCHED-03 UTF-8 title and prompt limits and explicit-target requirement", () => {
  const editor = { ...editorFor("project", now), title: "🦀".repeat(30), prompt: "Prompt" };
  expect(validate(editor, now).title).toBe(editor.title);
  expect(failure(() => validate({ ...editor, title: editor.title + "x" }, now)).includes("120 bytes")).toBe(true);
  expect(failure(() => validate({ ...editor, prompt: "a\tb" }, now)).includes("single-line prompt")).toBe(true);
  expect(failure(() => validate({ ...editor, fresh: false }, now)).includes("Choose an existing target")).toBe(true);
  expect(failure(() => validate({ ...editor, fresh: false, scope: { scope: "workspace", projectId: "project", worktreeId: null } }, now)).includes("Select a worktree")).toBe(true);
});

test("P6B-SCHED-04 scope and target changes require explicit selection, retaining typed kind", () => {
  const shell = row({ destination: "shell", options: null });
  const editor = editorFor("project", now, shell);
  expect(editor.pinned).toBe(true);
  expect(validate(editor, now).binding).toBe(null);
  const switched = selectScope(editor, { scope: "app", projectId: null, worktreeId: null });
  expect(switched.binding).toBe(null); expect(switched.pinned).toBe(false);
  const binding = { scope: switched.scope, kind: "shell", id: "same-id" };
  const selected = selectTarget(switched, binding);
  expect(validate(selected, now).binding).toEqual(binding);
  const targets = [{ binding, label: "Shell" }, { binding: { ...binding, kind: "legacy_chat" }, label: "Chat" }];
  expect(targetsFor(targets, selected).length).toBe(1);
  expect(targetsFor(targets, { ...selected, previous: row({ destination: "legacy_chat" }) }).length).toBe(2);
});

test("P6B-SCHED-05 app plus selected-project rows and actual run outcomes are retained", () => {
  const app = row({ key: { id: "app", revision: 1, scope: { scope: "app", projectId: null, worktreeId: null }, targetId: "global" } });
  const elsewhere = row({ key: { ...row().key, id: "other", scope: projectScope("other") } });
  expect(visibleRows([app, row(), elsewhere], "project").map((item) => item.key.id)).toEqual(["app", "schedule"]);
  expect(status(row({ paused: true, reviewRequired: true }))).toBe("Paused");
  expect(status(row({ nextRun: null }))).toBe("Complete");
  expect(optionsLabel(row()).includes("Permission: Supervised · Account: Claude system login")).toBe(true);
});

test("P6B-SCHED-06 pending actions dispatch once, revision refusal keeps the draft", async () => {
  const calls: string[] = [];
  let reject: (reason: unknown) => void = () => {};
  const api: ScheduleApi = {
    create: async () => row(), createChat: async () => row(),
    update: async (request) => { calls.push(`update:${request.key.revision}`); return await new Promise<ScheduleRow>((_, fail) => { reject = fail; }); },
    pause: async () => { calls.push("pause"); return row(); }, delete: async () => { calls.push("delete"); return row(); },
  };
  const model = new AutomationModel(api, () => {}, () => now); model.bind("project"); model.open(row());
  model.patch({ prompt: "Unsaved edit" });
  const saving = model.save();
  await model.save(); await model.pause(row()); model.cancel();
  expect(calls).toEqual(["update:4"]); expect(model.editor?.prompt).toBe("Unsaved edit");
  reject(new Error("revision_conflict: changed")); await saving;
  expect(model.error.includes("Reopen Edit")).toBe(true); expect(model.editor?.prompt).toBe("Unsaved edit");
  expect(model.pending).toBe(false);
  await model.remove(row()); expect(calls).toEqual(["update:4"]); expect(model.confirmDelete).toBe("schedule");
  await model.remove(row()); expect(calls).toEqual(["update:4", "delete"]); expect(model.confirmDelete).toBe(null);
});

test("P6B-SCHED-07 a project switch rejects a late mutation result and preserves its new draft", async () => {
  let finish: (value: ScheduleRow) => void = () => {};
  const api: ScheduleApi = { create: async () => row(), createChat: async () => await new Promise<ScheduleRow>((done) => { finish = done; }), update: async () => row(), pause: async () => row(), delete: async () => row() };
  const model = new AutomationModel(api, () => {}, () => now); model.bind("project"); model.open(); model.patch({ title: "First", prompt: "Prompt" });
  const saving = model.save(); model.bind("other"); finish(row()); await saving;
  expect(model.projectId).toBe("other"); expect(model.editor).toBe(null); expect(model.error).toBe("");
  model.open(); expect(model.editor?.scope).toEqual(projectScope("other"));
});
