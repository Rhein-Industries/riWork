import { View, host, mount, signal } from "nsh";
import { DEFAULT_THEME } from "../appearance";
import { CodeTerminalRow } from "../chat/CodeTerminalRow";

/** A code block's "Copy to:" row on fake terminals: the recorded provider
 * streams have no code blocks to open it from. `shell:broken` fails to copy;
 * `app.fixture.targets` replaces the open terminals, `app.fixture.open` opens
 * the row again. */
function Fixture() {
  const open = signal(true);
  const targets = signal([{ id: "shell:api", label: "zsh api" }, { id: "shell:broken", label: "zsh broken" }]);
  host.on("app.fixture.targets", (data) => targets.set(data as { id: string; label: string }[]));
  host.on("app.fixture.open", () => open.set(true));
  return <View style={{ flex: 1, padding: 12 }}>
    {open() && <CodeTerminalRow text="npm test" targets={targets} theme={() => DEFAULT_THEME} active={() => true} focus={true}
      onCopy={async (id, text) => {
        if (id === "shell:broken") throw new Error("Could not reach the terminal");
        host.post("app.fixture.copied", { id, text });
      }}
      onClose={(focused) => { open.set(false); host.post("app.fixture.closed", { focused }); }} />}
  </View>;
}

mount(Fixture);
