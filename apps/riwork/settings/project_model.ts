import type { FolderDto, ProjectDto } from "riwork/native";

export const SAVE_PROJECT_PROMPT = "Save changes  ↵ / CMD+S";
type Identity = Pick<ProjectDto, "id" | "name" | "folderId">;
export type MetadataSave = { id: string; name: string; folderId: string | null; version: number };

/** Local metadata edits live independently of immediately saved project choices. */
export class ProjectDraft {
  project: Identity;
  name: string;
  folderId: string | null;
  pending = false;
  nameTouched = false;
  folderTouched = false;
  notice = "";
  private version = 0;

  constructor(project: Identity) {
    this.project = project;
    this.name = project.name;
    this.folderId = project.folderId ?? null;
  }
  editName(name: string) { this.name = name; this.nameTouched = true; this.edited(); }
  chooseFolder(folderId: string | null) { this.folderId = folderId; this.folderTouched = true; this.edited(); }
  private edited() { this.version++; this.pending = true; this.notice = ""; }
  refresh(project: Identity, folders: readonly Pick<FolderDto, "id">[]) {
    if (project.id !== this.project.id) return;
    if (!this.nameTouched) this.name = project.name;
    if (!this.folderTouched) this.folderId = project.folderId ?? null;
    if (this.folderId !== null && !folders.some((folder) => folder.id === this.folderId)) this.folderId = null;
    this.project = project;
  }
  request(): MetadataSave { return { id: this.project.id, name: this.name, folderId: this.folderId, version: this.version }; }
  saved(request: MetadataSave, project: Identity) {
    if (request.id !== this.project.id || project.id !== this.project.id) return;
    this.project = project;
    if (request.version === this.version) {
      this.name = project.name; this.folderId = project.folderId ?? null;
      this.nameTouched = false; this.folderTouched = false; this.pending = false; this.notice = "Settings saved";
    }
  }
  immediateSaved(message: string) { if (!this.pending) this.notice = message; }
  message() { return this.pending || !this.notice ? SAVE_PROJECT_PROMPT : this.notice; }
}
