import { nativeComponent } from "nsh";

/** Schema 1: only identity and visibility cross JS. Frames, native text input,
 * mouse capture and release are owned by the registered AppKit component. */
export const PreviewSurface = nativeComponent<{ owner: string; visible: boolean }, { focusChange: { focused: boolean } }>("riwork.preview", {
  props: ["owner", "visible"], events: ["focusChange"],
});
