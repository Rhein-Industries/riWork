import { View, cleanup, effect, signal } from "nsh";
import { Flows, type FlowRunDto } from "riwork/native";
import type { InterfaceTheme } from "../appearance";
import { RunCard } from "./RunCard";

/** Above a chat's message box while a flow leads it: the step it is in, and Approve / Not yet
 * when the agent waits for the person. */
export function FlowStrip(props: { chatId: string; visible: () => boolean; theme: () => InterfaceTheme; notice: (message: string) => void }) {
  const run = signal<FlowRunDto | null>(null);
  const busy = signal(false);
  let alive = true;
  cleanup(() => { alive = false; });
  effect(() => {
    if (!props.visible()) return;
    const subscription = Flows.subscribeChat(props.chatId, (next) => { if (alive) run.set(next.run ?? null); });
    return () => subscription.close();
  });
  async function act(work: () => Promise<unknown>) {
    if (busy()) return;
    busy.set(true);
    try { await work(); } catch (reason) { if (alive) props.notice(String(reason)); } finally { if (alive) busy.set(false); }
  }
  return <View testID="chat-flow">
    {run() && <View style={{ marginBottom: 6 }}>
      <RunCard run={run()!} theme={props.theme} busy={busy} compact={true}
        onApprove={(note) => void act(() => Flows.approve(run()!.id, note))}
        onDeny={(note) => void act(() => Flows.deny(run()!.id, note))}
        onStop={() => void act(() => Flows.stop(run()!.id))} />
    </View>}
  </View>;
}
