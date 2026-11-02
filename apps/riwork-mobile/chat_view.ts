// A project's chat as an NSH component on iPad (`ios/RiWorkRemote/NSHChatComponent.swift`): the page draws it
// under its tab strip, beside the projects, as the Mac's window draws a chat tab. The transcript, the composer,
// approvals and questions, worker cards, dictation, file previews and the chat's keys stay native Swift
// (`ChatScreen`). The iPhone keeps the chat as a native screen over its page (`nav.present` `chat`).
import { nativeComponent } from "nsh";

export interface ChatTarget {
  /** The interactive host (a chat of any other host is `unavailable`). */
  desktopId: string;
  projectId: string;
  chatId: string;
}
export interface ChatProps {
  target: ChatTarget;
  /** On screen and taking input: false while a form, a menu or a native screen is over the page. */
  visible: boolean;
  /** Put the keyboard in the composer once the chat is up (a hardware keyboard is attached). */
  focusOnAttach: boolean;
}
export interface ChatEvents {
  /** `loading` until the chat's transcript is up, `ready`, or `unavailable` (with why: not listed on the Mac, another host). */
  state: { state: "loading" | "ready" | "unavailable"; message?: string };
  /** An app chord typed while the chat holds the keyboard (an `app_chords.ts` id). */
  command: { id: string };
  /** The chat's title as its tab should read it (an orchestrator's title, else the chat's). */
  title: { title: string };
}
/** Schema 1. The commands: `focus` and `blur` (the composer), `menu` (the chat's own actions, as its ⋯ had them
 * on the native screen: model, mode, notices, stop). */
export const ChatView = nativeComponent<ChatProps, ChatEvents>("riwork.chat", {
  props: ["target", "visible", "focusOnAttach"],
  events: ["state", "command", "title"],
});
export type ChatCommand = "focus" | "blur" | "menu";
