import { Text, View, host, mount, signal } from "nsh";
import { Chat, Handoff, Store } from "riwork/native";
import { ChatTab, type ChatDiagnostics } from "../chat/ChatTab";

/** A chat tab on the isolated fixture host. A new chat's working directory
 * names the recorded provider stream it replays; `chatId` reopens one. A
 * handoff in place shows the chat that took the tab, with the focus in its
 * message box, and opening another chat (a fork, the original) shows that one,
 * as a workspace would. */
function Fixture() {
  const params = host.params as { recording?: string; chatId?: string };
  const chatId = signal<string | null>(params.chatId ?? null);
  const focusNext = signal(false);
  const error = signal("");
  let last: ChatDiagnostics | null = null;
  if (!chatId()) {
    void Chat.fixtureDirectory(params.recording ?? "turn")
      .then(async (cwd) => {
        const projectId = params.recording === "image_turn"
          ? (await Store.addProject({ root: cwd, name: "Screenshot fixture" })).id : undefined;
        return Chat.create({ provider: "codex", cwd, projectId });
      })
      .then((chat) => chatId.set(chat.id))
      .catch((reason) => error.set(String(reason)));
  }
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", { chatId: chatId(), ...last }));
  // The chat's draft as the process-wide native store holds it now.
  host.on("app.fixture.draft", () => {
    const handle = Chat.open(chatId()!);
    void handle.restoreDraft()
      .then((draft) => host.post("app.fixture.draft", { text: draft?.text ?? null }))
      .catch((reason) => host.post("app.fixture.draft", { error: String(reason) }))
      .finally(() => handle.close());
  });
  // Lineage, made through the engine as the menus make it: a fork of the shown chat, or a
  // handoff of it to another agent. Each posts the new chat; `app.fixture.show` shows a chat.
  host.on("app.fixture.fork", (data) => {
    const request = (data ?? {}) as { at?: string; provider?: string; model?: string };
    void Chat.fork({ chatId: chatId()!, at: request.at ?? null, provider: request.provider ?? null, model: request.model ?? null })
      .then((fork) => host.post("app.fixture.lineage", { id: fork.id, context: fork.context }))
      .catch((reason) => host.post("app.fixture.lineage", { error: String(reason) }));
  });
  host.on("app.fixture.handoff", (data) => {
    const request = (data ?? {}) as { provider?: string; model?: string };
    void Handoff.run({ sourceId: chatId()!, kind: "chat", provider: request.provider ?? "claude", model: request.model ?? null, context: "transcript", inPlace: true })
      .then((handoff) => host.post("app.fixture.lineage", { id: handoff.id, context: handoff.context }))
      .catch((reason) => host.post("app.fixture.lineage", { error: String(reason) }));
  });
  host.on("app.fixture.show", (data) => { const id = (data as { chatId?: string } | undefined)?.chatId; if (id) chatId.set(id); });
  return <View style={{ flex: 1 }}>
    {error() !== "" && <Text testID="fixture-error">{error()}</Text>}
    {chatId() && <ChatTab chatId={chatId()!} visible={true} onDiagnostics={(report) => { last = report; }}
      focusOnAttach={focusNext()} onAttached={() => focusNext.set(false)}
      onReplace={(id) => { focusNext.set(true); chatId.set(id); }} onOpenWorker={(id, kind) => { if (kind === "chat") chatId.set(id); }} />}
  </View>;
}

mount(Fixture);
