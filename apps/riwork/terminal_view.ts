// The terminal's declaration, shared by the Mac (Ghostty, crates/riwork-nsh-terminal) and iPhone/iPad (the
// capture terminal, ios/RiWorkRemote). It imports nothing Mac-only, so the mobile bundle can load it.
import { nativeComponent } from "nsh";

export type TerminalTarget = { kind: "local"; shellId: string } | { kind: "remote"; desktopId: string; shellId: string };
export type TerminalColors = { source: "ghostty" } | { source: "override"; background: string; foreground: string; palette: string[] };
export type TerminalState = "detached" | "attaching" | "attached" | "failed" | "claimed" | "reconnecting" | "ended" | "gone";
export interface TerminalProps {
  target: TerminalTarget;
  visible: boolean;
  pinned: boolean;
  focusOnAttach: boolean;
  colors: TerminalColors;
  frozen: boolean;
  /** Schema 1.1, iOS only: 1 is the device's saved terminal size. */
  fontScale?: number;
  /** Schema 1.2, Mac local shells only: set while the window switches project, so the terminal an unmount
   * then removes is kept for the next view of its shell instead of detached. */
  keepOnUnmount?: boolean;
}
export interface TerminalEvents {
  state: { state: TerminalState; message?: string; attempts?: number };
  focusChange: { focused: boolean };
  linkOpen: { action: "open" | "edit"; kind: "url" | "file" | "folder"; target: string; line?: number; column?: number };
  drop: { paths: string[]; images: string[]; accepted: boolean; reason?: string };
  inputRefused: { reason: string };
  findResult: { total: number; current: number };
  findOpen: { query: string };
  /** Schema 1.1, iOS only: an app chord typed while the terminal holds the keyboard. */
  command: { id: string };
  /** Schema 1.1, iOS only: the shell's title changed. */
  title: { title: string };
}
/** Schema 1, with the dated 1.1 and 1.2 amendments (docs/nsh-contracts.md §3): input, drawing and link
 * detection stay in the native view. The Mac's view ignores `fontScale` and never sends `command` or `title`;
 * the iOS view takes only remote targets and ignores `keepOnUnmount`. */
export const Terminal = nativeComponent<TerminalProps, TerminalEvents>("riwork.terminal", {
  props: ["target", "visible", "pinned", "focusOnAttach", "colors", "frozen", "fontScale", "keepOnUnmount"],
  events: ["state", "focusChange", "linkOpen", "drop", "inputRefused", "findResult", "findOpen", "command", "title"],
});
export type TerminalCommand = "focus" | "blur" | "reattach" | "copy" | "paste" | "selectAll" | "find" | "findNext" | "findPrevious" | "findClose";


export function terminalStateMessage(state: TerminalEvents["state"]): string {
  if (state.message) return state.message;
  switch (state.state) {
    case "attaching": return "Connecting to session…";
    case "failed": return "Could not attach to this session.";
    case "claimed": return "This session is displayed in another window.";
    case "gone": return "This session has ended.";
    case "ended": return "Session ended.";
    case "reconnecting": return "Reconnecting…";
    default: return "";
  }
}
