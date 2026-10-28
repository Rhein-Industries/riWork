/** Automation presentation and explicit-action state. No timers or dispatch. */
export interface Scope { scope: string; projectId: string | null; worktreeId: string | null }
export interface Key { id: string; revision: number; scope: Scope; targetId: string }
export interface ChatOptions { provider: string; model: string | null; effort: string | null; fast: boolean; permission: string; accountId: string | null }
export interface Binding { scope: Scope; kind: string; id: string }
export interface Run { dueAt: number; observedAt: number; dueDisplay: string; observedDisplay: string; outcome: string; message: string; createdChatId: string | null }
export interface ScheduleRow {
  key: Key; title: string; prompt: string; destination: string; destinationLabel: string;
  options: ChatOptions | null; accountLabel: string; firstAt: number; firstIso: string;
  everyMinutes: number | null; paused: boolean; reviewRequired: boolean;
  nextRun: number | null; nextDisplay: string | null; lastRun: Run | null;
}
export interface Target { binding: Binding; label: string }
export interface ScheduleData {
  schema: number; now: number; rows: ScheduleRow[]; targets: Target[];
  chats: { id: string; projectId: string; title: string }[];
  worktrees: { id: string; projectId: string; label: string }[];
  error: string | null; runtimeError: string | null;
}
export interface CreateRequest {
  scope: Scope; title: string; prompt: string; at: string; everyMinutes: number | null;
  binding: Binding | null; options: ChatOptions | null;
}
export interface UpdateRequest extends Omit<CreateRequest, "scope"> { key: Key }
export interface ScheduleApi {
  create(request: CreateRequest): Promise<ScheduleRow>;
  createChat(request: CreateRequest): Promise<ScheduleRow>;
  update(request: UpdateRequest): Promise<ScheduleRow>;
  pause(key: Key, paused: boolean): Promise<ScheduleRow>;
  delete(key: Key): Promise<ScheduleRow>;
}
export interface Editor {
  previous: ScheduleRow | null; fresh: boolean; scope: Scope; binding: Binding | null;
  pinned: boolean; title: string; prompt: string; at: string; exact: boolean;
  quick: number | null; repeat: "once" | "hourly" | "daily" | "weekly" | "minutes";
  minutes: string; options: ChatOptions;
}
export const QUICK_TIMES = [[600, "In 10 min"], [1800, "In 30 min"], [3600, "In 1 hour"], [86400, "In 1 day"]] as const;
export const PERMISSIONS = [["supervised", "Supervised"], ["auto-edit", "Auto edit"], ["full", "Full access"], ["plan", "Plan"]] as const;
export const FOOTNOTE = "Runs while RiWork is open. Busy/blocked targets defer for up to 5 min; older runs are skipped. At most 4 attempts/min. Uncertain sends pause without retry. Edit and save a future run to resume after review. TAB / ENTER · CMD+S to save.";
export const EMPTY = "No automations yet. Schedule a prompt to start a new project chat, or select an existing AI shell.";
export const EMPTY_TARGETS = "No live Codex or Claude session in this scope. Open one separately, complete a turn, then return here.";
export function defaults(): ChatOptions { return { provider: "codex", model: null, effort: null, fast: false, permission: "supervised", accountId: null }; }
export function projectScope(projectId: string): Scope { return { scope: "project", projectId, worktreeId: null }; }
export function sameScope(a: Scope, b: Scope): boolean { return a.scope === b.scope && a.projectId === b.projectId && a.worktreeId === b.worktreeId; }
export function visibleRows(rows: ScheduleRow[], projectId: string): ScheduleRow[] { return rows.filter((row) => row.key.scope.scope === "app" || row.key.scope.projectId === projectId); }
export function targetsFor(targets: Target[], editor: Editor): Target[] {
  return targets.filter((target) => sameScope(target.binding.scope, editor.scope) &&
    (target.binding.kind === "shell" || editor.previous?.destination === "legacy_chat"));
}
export function localIso(unix: number): string {
  const date = new Date(unix * 1000), pad = (n: number) => String(n).padStart(2, "0");
  const offset = -date.getTimezoneOffset(), sign = offset < 0 ? "-" : "+", minutes = Math.abs(offset);
  return `${String(date.getFullYear()).padStart(4, "0")}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}${sign}${pad(Math.floor(minutes / 60))}:${pad(minutes % 60)}`;
}
export function editorFor(projectId: string, now: number, row?: ScheduleRow): Editor {
  const minutes = row?.everyMinutes ?? null;
  return {
    previous: row ?? null, fresh: !row || row.destination === "new_chat", scope: row?.key.scope ?? projectScope(projectId),
    binding: row && row.destination !== "new_chat" ? { scope: row.key.scope, kind: row.destination === "legacy_chat" ? "legacy_chat" : "shell", id: row.key.targetId } : null,
    pinned: !!row, title: row?.title ?? "", prompt: row?.prompt ?? "",
    at: localIso(row?.nextRun != null && row.nextRun > now ? row.nextRun : now + 600), exact: !!row,
    quick: row ? null : 600,
    repeat: minutes == null ? "once" : minutes === 60 ? "hourly" : minutes === 1440 ? "daily" : minutes === 10080 ? "weekly" : "minutes",
    minutes: String(minutes ?? 60), options: row?.options ? { ...row.options } : defaults(),
  };
}
export function selectScope(editor: Editor, scope: Scope): Editor { return { ...editor, scope, binding: null, pinned: false }; }
export function selectTarget(editor: Editor, binding: Binding): Editor { return { ...editor, binding, pinned: false }; }
export function quickTime(editor: Editor, seconds: number, now: number): Editor { return { ...editor, quick: seconds, at: localIso(now + seconds) }; }
export function singleLine(text: string): string { return text.replace(/\r\n?|\n|\u2028|\u2029/g, " "); }
function utf8Bytes(text: string): number { let bytes = 0; for (const char of text) { const point = char.codePointAt(0)!; bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4; } return bytes; }
export function repeatMinutes(editor: Editor): number | null {
  if (editor.repeat === "once") return null;
  if (editor.repeat === "hourly") return 60;
  if (editor.repeat === "daily") return 1440;
  if (editor.repeat === "weekly") return 10080;
  if (!editor.minutes.trim()) throw new Error("Enter repeat minutes");
  const value = Number(editor.minutes);
  if (!/^\d+$/.test(editor.minutes.trim()) || !Number.isSafeInteger(value) || value < 5 || value > 525600)
    throw new Error("Repeat interval must be whole minutes, from 5 minutes to 365 days");
  return value;
}
export function validate(editor: Editor, now: number): CreateRequest {
  if (!editor.title.trim() || utf8Bytes(editor.title) > 120 || /[\x00-\x1f\x7f-\x9f\u2028\u2029]/.test(editor.title.trim())) throw new Error("Enter a title (up to 120 bytes, no control characters)");
  if (!editor.prompt.trim() || utf8Bytes(editor.prompt) > 16384 || /[\x00-\x1f\x7f-\x9f\u2028\u2029]/.test(editor.prompt)) throw new Error("Enter a single-line prompt (up to 16 KiB, no control characters)");
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:Z|[+-]\d{2}:\d{2})$/.test(editor.at.trim())) throw new Error("Use YYYY-MM-DDTHH:MM:SS+HH:MM with an explicit UTC offset");
  const at = Date.parse(editor.at.trim()) / 1000;
  if (!Number.isFinite(at)) throw new Error("Use YYYY-MM-DDTHH:MM:SS+HH:MM with an explicit UTC offset");
  if (at < 0) throw new Error("Date must be after 1970");
  if (at > 253402300799) throw new Error("Date must be before year 10000");
  if (at <= now) throw new Error("First run must be in the future");
  if (editor.scope.scope === "workspace" && !editor.scope.worktreeId) throw new Error("Select a worktree in WORKTREES before creating a workspace schedule");
  if (!editor.fresh && !editor.binding) throw new Error("Choose an existing target session");
  if (editor.fresh && editor.scope.scope !== "project") throw new Error("New chats require project scope");
  return { scope: editor.scope, title: editor.title, prompt: editor.prompt, at: editor.at.trim(), everyMinutes: repeatMinutes(editor),
    binding: editor.fresh || editor.pinned ? null : editor.binding, options: editor.fresh ? { ...editor.options, model: editor.options.model?.trim() || null, effort: editor.options.effort?.trim() || null } : null };
}
export function status(row: ScheduleRow): string { return row.paused ? "Paused" : row.nextRun == null ? "Complete" : "Active"; }
export function permissionLabel(value: string): string { return PERMISSIONS.find(([key]) => key === value)?.[1] ?? value; }
export function optionsLabel(row: ScheduleRow): string {
  const options = row.options;
  return `Permission: ${options ? permissionLabel(options.permission) : "Pinned with target"} · Account: ${row.accountLabel} · Model: ${options?.model ?? "Default"} · Effort: ${options?.effort ?? "Default"}${options?.fast ? " · Fast" : ""}`;
}
export function errorMessage(reason: unknown): string {
  const message = String(reason).replace(/^Error: /, "");
  return message.includes("revision_conflict:") ? `${message} Reopen Edit to review the current revision; your draft has been kept.` : message;
}

/** A pending operation cannot be activated twice or apply into a different project. */
export class AutomationModel {
  editor: Editor | null = null;
  pending = false;
  confirmDelete: string | null = null;
  error = "";
  projectId = "";
  data: ScheduleData | null = null;
  private generation = 0;
  constructor(private api: ScheduleApi, private changed: () => void, private now: () => number = () => Math.floor(Date.now() / 1000)) {}
  bind(projectId: string): void {
    if (this.projectId === projectId) return;
    this.projectId = projectId; this.generation++; this.editor = null; this.confirmDelete = null; this.error = ""; this.data = null; this.changed();
  }
  receive(data: ScheduleData): void { this.data = data; this.changed(); }
  patch(patch: Partial<Editor>): void { if (!this.pending && this.editor) { this.editor = { ...this.editor, ...patch }; this.changed(); } }
  open(row?: ScheduleRow): void { if (!this.pending) { this.editor = editorFor(this.projectId, this.now(), row); this.confirmDelete = null; this.error = ""; this.changed(); } }
  cancel(): void { if (!this.pending) { this.editor = null; this.confirmDelete = null; this.error = ""; this.changed(); } }
  private async run(work: () => Promise<unknown>, saved = false): Promise<void> {
    if (this.pending) return;
    const token = this.generation;
    this.pending = true; this.error = ""; this.changed();
    try { await work(); if (token === this.generation && saved) this.editor = null; }
    catch (reason) { if (token === this.generation) this.error = errorMessage(reason); }
    finally { this.pending = false; this.confirmDelete = null; this.changed(); }
  }
  async save(): Promise<void> {
    if (this.pending || !this.editor) return;
    const editor = this.editor;
    let request: CreateRequest;
    try { request = validate(editor, this.now()); }
    catch (reason) { this.error = errorMessage(reason); this.changed(); return; }
    await this.run(() => editor.previous ? this.api.update({ key: editor.previous.key, title: request.title, prompt: request.prompt, at: request.at, everyMinutes: request.everyMinutes, binding: request.binding, options: request.options })
      : editor.fresh ? this.api.createChat(request) : this.api.create(request), true);
  }
  async pause(row: ScheduleRow): Promise<void> { await this.run(() => this.api.pause(row.key, !row.paused)); }
  async remove(row: ScheduleRow): Promise<void> {
    if (this.pending) return;
    if (this.confirmDelete !== row.key.id) { this.confirmDelete = row.key.id; this.changed(); return; }
    await this.run(() => this.api.delete(row.key));
  }
}
