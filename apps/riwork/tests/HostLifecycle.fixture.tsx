import { host } from "nsh";

/** A one-shot fixture lifetime; production subscriptions never use this timer.
 * The entry imports the real App before scheduling, allowing its native layout
 * session and workspace to mount during the three-second startup grace period.
 * No custom host params or desktop input are required.
 */
export function scheduleLifecycle(action: "window.close" | "app.quit"): void {
  const params = host.params as { windowId?: string };
  setTimeout(() => {
    console.log(`[host-lifecycle-fixture] ${action} requested for ${params.windowId ?? "unknown window"}`);
    void host.call(action, {}).then(() => {
      console.log(`[host-lifecycle-fixture] ${action} accepted`);
    }).catch((error: unknown) => {
      console.error(`[host-lifecycle-fixture] ${action} failed: ${String(error)}`);
    });
  }, 3000);
}
