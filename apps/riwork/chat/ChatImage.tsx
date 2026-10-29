import { Image, Text, View, cleanup, signal } from "nsh";
import type { ChatImagePreview } from "riwork/native";
import type { InterfaceTheme } from "../appearance";

// Several images can share a visible activity row. Limit concurrent native
// decoders, and discard queued work whose List cell has since been disposed.
let reading = 0;
const waiting: (() => void)[] = [];
function schedule(read: () => Promise<void>) {
  waiting.push(() => {
    reading++;
    void read().finally(() => { reading--; drain(); });
  });
  drain();
}
function drain() {
  while (reading < 2 && waiting.length) waiting.shift()!();
}

/** A mounted List cell owns its preview bytes. Embedded images are safe to load
 * immediately; local files still need a click and the native chat scope check.
 * `cache` is the cell's: a card made again in the same cell (its row gained an
 * image) shows what it read at once, instead of loading it again. */
export function ChatImage(props: {
  id: string;
  label: string;
  inline: boolean;
  preview: () => Promise<ChatImagePreview>;
  cache?: Map<string, ChatImagePreview>;
  theme: () => InterfaceTheme;
  width: () => number;
  expand: (preview: ChatImagePreview) => void;
  openLink: (url: string) => void;
}) {
  const cached = props.cache?.get(props.id);
  const preview = signal<ChatImagePreview | null>(cached ?? null);
  const loading = signal(false);
  const error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  function load() {
    if (loading()) return;
    loading.set(true); error.set("");
    const id = props.id;
    schedule(async () => {
      if (!alive) return;
      try {
        const value = await props.preview();
        props.cache?.set(id, value);
        if (alive) preview.set(value);
      } catch (reason) {
        if (alive) error.set(String(reason));
      } finally { if (alive) loading.set(false); }
    });
  }
  if (props.inline && !cached) void load();
  const width = () => Math.max(1, Math.min(560, props.width()));
  const height = () => Math.min(420, width() * ((preview()?.height || 3) / (preview()?.width || 4)));
  const text = () => ({ fontSize: 12 * props.theme().scale, color: props.theme().palette.muted });
  return <View testID={`chat-image-card:${props.id}`} style={{ maxWidth: "100%", gap: 5, paddingVertical: 4 }}>
    {preview()?.dataUrl && !error() && <View role="button" accessibilityLabel={`Expand ${props.label || "image"}`}
      onPress={() => props.expand(preview()!)}>
      <Image testID={`chat-image:${props.id}`} src={preview()!.dataUrl!} resizeMode="contain"
        accessibilityLabel={props.label || "Image"} style={{ width: width(), height: height() }}
        onError={(event) => { props.cache?.delete(props.id); error.set(event.message || "This image could not be displayed"); }} />
    </View>}
    {loading() && <Text style={text()}>Loading image…</Text>}
    {(error() || preview()?.message) && <Text testID={`chat-image-error:${props.id}`} style={text()}>{error() || preview()!.message!}</Text>}
    {preview()?.url && <View role="button" accessibilityLabel="Open image" onPress={() => props.openLink(preview()!.url!)}><Text style={text()}>Open image</Text></View>}
    {(!preview() || error()) && !loading() && <View role="button" testID={`chat-image-load:${props.id}`}
      accessibilityLabel={`Preview ${props.label || "image"}`} onPress={() => void load()}><Text style={text()}>{error() ? "Retry image" : `Preview ${props.label || "image"}`}</Text></View>}
  </View>;
}
