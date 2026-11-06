# App previews

[Documentation index](../README.md) · [Project overview](../../README.md)

Repository paths and commands below are relative to the checkout root.

## App preview tabs

Choose **App preview…** in a pane's **+** menu or **View → App preview…**, enter the app's launch command and working folder, and choose **Run**. **Hidden GPUI** runs a separate process with no native window or Dock icon, streams compressed frames into the tab, and forwards pointer, scroll, keyboard, and committed text input. Add the development adapter described in [riwork-gpui-preview](../../crates/riwork-gpui-preview/README.md) to your GPUI app. To try the included example in this checkout, use `cargo run -p riwork-gpui-preview --example counter` as the command and this checkout as the working folder.

Tabs save their command, folder, and mode, and restore stopped. **Restart** rebuilds/reruns the command; **Stop** and closing a tab stop its launched process group. Rendering pauses when the tab or Somtus window is inactive. Logs are available below the preview. The GPUI adapter currently uses the real Metal renderer through GPUI's development headless platform; native dialogs, clipboard, accessibility, and native IME composition inside the child are not supplied by that platform.

**Cua window** is an optional mode for an unmodified local app, using Somtus's guarded Cua.ai Driver for capture and input. It targets a window of the process launched by this tab. This mode requires a normal, non-minimized app window; it does not make arbitrary applications windowless or remove their Dock icons. Missing driver or macOS permissions are reported with the Somtus Cua setup instructions.

**iOS Simulator** builds an Xcode scheme, creates a dedicated simulator, installs and launches the app, and streams its framebuffer with touch input into the tab without opening Simulator.app. Choose the working folder and scheme; project, iPhone model, and installed iOS version can be selected or detected automatically. Stop and closing the tab shut down and delete only its owned simulator. The native adapter has been verified with Xcode 26.0.1 on iOS 18.1 and 26.0; it currently supports one finger, scrolling, Home, and US keyboard text. See the [native adapter and real-device proof](../../tools/ios-preview-bridge/README.md) for compatibility and input limits.

Coding shells can create and reuse these tabs, wait for a first frame, read build logs, send touch input, capture a fresh frame, and close them through `somtus preview` or the `riwork_preview_*` MCP tools. For example: `somtus preview open --ios --scheme MyApp --wait --json`. Commands target an exact preview ID and work while its window is inactive. See [preview CLI and MCP controls](../../docs/preview-control.md).
