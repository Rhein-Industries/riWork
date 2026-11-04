/** New project: a name, and whether the Mac makes it a Git repository. The Mac makes it in its default
 * projects folder (`project.create`); the phone never names a path. One request at a time, never repeated:
 * an answer that was lost says the project may have been made and reads the list again. */
import { Text, View, signal } from "nsh";
import { Dialog, Input } from "nsh/base";
import { PortError } from "../../riwork/client/ports";
import type { ProjectSummary } from "../../riwork/client/types";
import type { MobileApp } from "../app_state";
import type { Kit } from "../ui";

/** The Mac's rules for a project name (`remote/src/rpc.rs` `project_name`, `ios/Core/NewProject.swift`). */
export function projectNameProblem(name: string): string | null {
  const scalars = [...name];
  if (!scalars.length) return "Enter a name for the project.";
  if (/^\s|\s$/u.test(name)) return "A project name cannot start or end with a space.";
  if ([...name].some(isControl)) return "A project name is one line without control characters.";
  if (scalars.length > 100) return "A project name is at most 100 characters.";
  if (utf8Length(name) > 255) return "That name is too long for a folder name on the Mac.";
  if (/[/\\]/.test(name)) return "A project name cannot contain “/” or “\\”.";
  if (name.startsWith(".")) return "A project name cannot start with a dot.";
  if (name.startsWith("-")) return "A project name cannot start with “-”.";
  return null;
}

/** C0 and C1 controls and the line and paragraph separators (`KeyItem.isForbidden` on the phone, the Mac's rule). */
function isControl(char: string): boolean {
  const code = char.codePointAt(0)!;
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f) || code === 0x2028 || code === 0x2029;
}

function utf8Length(text: string): number {
  let bytes = 0;
  for (const char of text) { const code = char.codePointAt(0)!; bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4; }
  return bytes;
}

export function NewProjectForm(props: {
  app: MobileApp; ui: Kit; host: string; open: boolean; onClose: () => void;
  onCreated: (project: ProjectSummary) => void; onUncertain: () => void;
}) {
  const { app, ui } = props;
  const { Action, Toggle, palette, pt, target, text } = ui;
  const name = signal(""), git = signal(true), busy = signal(false), attempted = signal(false);
  const message = signal<{ text: string; error: boolean } | null>(null);
  const trimmed = () => name().trim();
  const problem = () => trimmed() || attempted() ? projectNameProblem(trimmed()) : null;
  const connected = () => app.link(props.host).state === "connected";
  function close() {
    if (busy()) return;
    name.set(""); git.set(true); attempted.set(false); message.set(null);
    props.onClose();
  }
  async function create() {
    if (busy()) return;
    attempted.set(true);
    if (projectNameProblem(trimmed())) return;
    const create = app.services(props.host).projects.create;
    if (!create) { message.set({ text: "Update Somtus on the Mac to create projects from here.", error: true }); return; }
    busy.set(true); message.set(null);
    try {
      const project = await create(trimmed(), git());
      busy.set(false);
      close();
      props.onCreated(project);
    } catch (cause) {
      busy.set(false);
      const failure = PortError.from(cause);
      if (failure.code === "uncertain") {
        // Asking again would only answer "already exists" if it was made: the list says.
        message.set({ text: "The Mac may have made this project, but its answer was lost. The project list was read again: look for it there before making it again.", error: true });
        props.onUncertain();
      } else if (failure.code === "conflict") message.set({ text: "A project with this name already exists on the Mac. Choose another name.", error: true });
      else if (failure.code === "unsupported") message.set({ text: "Update Somtus on the Mac to create projects from here.", error: true });
      else message.set({ text: failure.message, error: true });
    }
  }
  return <Dialog open={props.open} presentation="formSheet" onOpenChange={(open) => { if (!open) close(); }} closeOnSwipe={!busy()} closeOnEscape={!busy()}
    style={{ backgroundColor: palette().bg, padding: pt(20), gap: pt(16) }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: pt(8) }}>
      <Action label="Cancel" compact={true} disabled={busy()} action={close} />
      <Text role="header" style={{ ...text(17), fontWeight: "600", flex: 1, textAlign: "center" }}>New project</Text>
      <Action testID="new-project-create" label={busy() ? "Creating…" : "Create"} compact={true} prominent={true}
        disabled={busy() || !connected() || !!projectNameProblem(trimmed())} action={() => void create()} />
    </View>
    <Input testID="new-project-name" accessibilityLabel="Project name" placeholder="Name" value={name()} autoFocus={true}
      onChange={(value) => { name.set(value); message.set(null); }} onSubmit={() => void create()} returnKeyType="done"
      autoCapitalize="words" autoCorrect={false} readonly={busy()}
      style={{ ...text(16), minHeight: target(), paddingHorizontal: pt(14), paddingVertical: pt(10), backgroundColor: palette().panel, borderRadius: pt(11) }} />
    {problem() && <Text style={text(13, "error")}>{problem()!}</Text>}
    <Toggle label="Git repository" detail="The Mac runs git init in the new folder." checked={git()} disabled={busy()} onChange={(value) => git.set(value)} />
    <Text style={text(12, "muted")}>{connected() ? `In ${app.hostName(props.host)}'s projects folder.` : "Connect to this desktop to create a project."}</Text>
    {message() && <Text style={text(13, message()!.error ? "error" : "muted")}>{message()!.text}</Text>}
  </Dialog>;
}
