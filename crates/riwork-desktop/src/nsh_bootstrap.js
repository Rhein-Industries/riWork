// Temporary Phase 1b shell; the same bundle runs in every AppView.
// RIWORK_NSH_BUNDLE selects the future TSX app's built bundle.js.
import { component } from "nsh:rt";
import { Text, View, host, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { Window } from "nsh/desktop";
import "riwork/native";

const params = host.params;
const status = signal("");
const title = params.kind === "global" ? "RiWork — Global" : "RiWork — Project";
Window.setTitle(title);
Window.setTitleBar("hidden");
host.on("command", ({ id }) => status.set(`Command: ${id}`));
host.on("global.show", ({ view }) => status.set(`Global view: ${view}`));
host.on("route", ({ step }) => status.set(`Route: ${step.kind ?? step.type ?? JSON.stringify(step)}`));
host.on("project.open", () => status.set("Project opened"));

mount(() => component(View, {
  style: { flex: 1, padding: 40, gap: 16 },
  children: [
    component(Text, { style: { fontSize: 24, fontWeight: "bold" }, children: title }),
    component(Text, { children: "AppKit / NSH host · Phase 1b" }),
    component(Text, { children: `Window: ${params.windowId}` }),
    component(Text, { children: `Project: ${params.projectId ?? "global"}` }),
    component(Text, { children: params.path }),
    component(Button, { onPress: () => host.call("window.open", { kind: "global", view: "dashboard" }), children: component(Text, { children: "Open Global Dashboard" }) }),
    component(Text, { children: () => status() }),
  ],
}));
// P1a/TSX must send this only once its Rust LayoutSession has restored.
host.post("app.restoreReady", {});
