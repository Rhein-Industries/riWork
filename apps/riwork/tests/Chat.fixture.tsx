import { Text, View, host, mount, signal } from "nsh";
import { Chat } from "riwork/native";
import { ChatTab, type ChatDiagnostics } from "../chat/ChatTab";

/** A chat tab on the isolated fixture host. A new chat's working directory
 * names the recorded provider stream it replays; `chatId` reopens one. */
function Fixture() {
  const params = host.params as { recording?: string; chatId?: string };
  const chatId = signal<string | null>(params.chatId ?? null);
  const error = signal("");
  let last: ChatDiagnostics | null = null;
  if (!chatId()) {
    void Chat.fixtureDirectory(params.recording ?? "turn")
      .then((cwd) => Chat.create({ provider: "codex", cwd }))
      .then((chat) => chatId.set(chat.id))
      .catch((reason) => error.set(String(reason)));
  }
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", { chatId: chatId(), ...last }));
  return <View style={{ flex: 1 }}>
    {error() !== "" && <Text testID="fixture-error">{error()}</Text>}
    {chatId() && <ChatTab chatId={chatId()!} visible={true} onDiagnostics={(report) => { last = report; }} />}
  </View>;
}

mount(Fixture);
