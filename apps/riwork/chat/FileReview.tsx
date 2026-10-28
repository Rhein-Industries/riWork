import { Clipboard, List, Scroll, Text, View, cleanup, computed, effect, keymap, signal, type YNode } from "nsh";
import { Input } from "nsh/base";
import type { ChatChangedFile, ChatFileReview, ChatHandle } from "riwork/native";
import type { InterfaceTheme } from "../appearance";

/** A provider patch reader. Opening a live file is separate from reading its recorded edit. */
export function FileReview(props: {
  handle: ChatHandle; scope: () => string | undefined; initial: () => string | undefined;
  theme: () => InterfaceTheme; active: () => boolean; onClose: () => void;
  onOpen: (path: string, line?: number) => void; onError: (message: string) => void;
}) {
  const files = signal<ChatChangedFile[]>([]), selected = signal<string | null>(null), review = signal<ChatFileReview | null>(null);
  const readError = signal(""), reading = signal(false), retry = signal(0);
  const loading = signal(true), filter = signal(""), width = signal(0), hunk = signal(-1);
  let list: YNode | undefined, alive = true, generation = 0;
  cleanup(() => { alive = false; generation++; });
  effect(() => {
    const scope = props.scope(), initial = props.initial();
    const token = ++generation; loading.set(true); review.set(null); selected.set(null); readError.set("");
    void props.handle.changedFiles(scope).then((entries) => {
      if (!alive || token !== generation) return;
      files.set(entries); selected.set(entries.find((entry) => entry.id === initial)?.id ?? entries[0]?.id ?? null);
      loading.set(false);
    }).catch((error) => { if (alive && token === generation) { loading.set(false); readError.set(String(error)); props.onError(String(error)); } });
  });
  let readGeneration = 0;
  effect(() => {
    const id = selected(), token = ++readGeneration; retry();
    review.set(null); hunk.set(-1); reading.set(false);
    if (!id) return;
    readError.set(""); reading.set(true);
    void props.handle.fileReview(id).then((value) => { if (alive && token === readGeneration) { review.set(value); reading.set(false); list?.scrollToIndex({ index: 0 }); } })
      .catch((error) => { if (alive && token === readGeneration) { reading.set(false); readError.set(String(error)); props.onError(String(error)); } });
  });
  effect(() => { if (props.active()) return keymap.bind("escape", props.onClose, { when: "always" }); });
  const theme = props.theme;
  const text = (size = 11, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  const button = (label: string, action: () => void, disabled = false) => <View role="button" accessibilityLabel={label}
    accessibilityState={{ disabled }} onPress={() => { if (!disabled) action(); }} style={{ paddingHorizontal: 8, paddingVertical: 6, borderRadius: 4, opacity: disabled ? 0.4 : 1 }}
    hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Text style={text(11, theme().palette.cyan)}>{label}</Text></View>;
  const shown = () => files().filter((file) => file.path.toLowerCase().includes(filter().trim().toLowerCase()));
  const hunks = computed(() => review()?.lines.flatMap((line, at) => line.kind === "hunk" ? [at] : []) ?? []);
  function moveHunk(delta: number) {
    const entries = hunks(); if (!entries.length) return;
    const next = Math.max(0, Math.min(entries.length - 1, hunk() + delta)); hunk.set(next); list?.scrollToIndex({ index: entries[next], viewPosition: 0 });
  }
  return <View onLayout={(bounds) => width.set(bounds.size.width)} style={{ position: "absolute", top: 0, bottom: 0, left: 0, right: 0, backgroundColor: theme().palette.bg, zIndex: 20 }}>
    <View style={{ flexDirection: "row", alignItems: "center", padding: 8, gap: 5, borderBottomWidth: 1, borderColor: theme().palette.divider }}>
      <View style={{ flex: 1 }}><Text style={text(13)}>Reported file changes</Text><Text style={text(10, theme().palette.muted)}>{`${files().length} ${files().length === 1 ? "file" : "files"} · provider patches`}</Text></View>
      {button("Close", props.onClose)}
    </View>
    <View style={{ flex: 1, minHeight: 0, flexDirection: width() > 580 ? "row" : "column" }}>
      <View style={{ width: width() > 580 ? 230 : "100%", maxHeight: width() > 580 ? undefined : 150, borderRightWidth: width() > 580 ? 1 : 0, borderColor: theme().palette.divider, padding: 6, gap: 5 }}>
        <Input value={filter()} onChange={(value) => filter.set(value)} placeholder="Filter files" accessibilityLabel="Filter changed files"
          style={{ ...text(11), padding: 6, borderWidth: 1, borderColor: theme().palette.divider, borderRadius: 4 }} />
        <Scroll style={{ flex: 1, minHeight: 0 }}>
          {shown().map((file) => <View role="button" accessibilityLabel={file.path} accessibilityState={{ selected: selected() === file.id }} onPress={() => selected.set(file.id)}
            style={{ padding: 8, gap: 4, borderRadius: 4, backgroundColor: selected() === file.id ? theme().palette.panelActive : "transparent" }}>
            <Text numberOfLines={2} style={{ ...text(11), fontFamily: theme().monoFamily }}>{file.path}</Text>
            <Text style={text(10, theme().palette.muted)}>{`${file.kind} · +${file.added} −${file.removed}`}</Text>
          </View>)}
        </Scroll>
      </View>
      <View style={{ flex: 1, minWidth: 0, minHeight: 0 }}>
        {review() && <>
          <View style={{ padding: 8, gap: 4, borderBottomWidth: 1, borderColor: theme().palette.divider }}>
            <Text selectable={true} style={{ ...text(12), fontFamily: theme().monoFamily }}>{review()!.file.path}</Text>
            <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 3 }}>
              {button("Open file", () => props.onOpen(review()!.file.path))}
              {button("Copy patch", () => void Clipboard.writeText(review()!.patch), !review()!.patch)}
              {button("Previous hunk", () => moveHunk(-1), !hunks().length || hunk() <= 0)}
              {button("Next hunk", () => moveHunk(1), !hunks().length || hunk() >= hunks().length - 1)}
              <Text style={text(10, theme().palette.muted)}>{hunks().length ? hunk() < 0 ? `${hunks().length} hunks` : `${hunk() + 1} / ${hunks().length}` : ""}</Text>
            </View>
          </View>
          {!review()!.file.hasDiff && <Text style={{ ...text(12, theme().palette.muted), padding: 16 }}>The provider reported this change without a patch.</Text>}
          <List ref={(node) => { list = node; }} each={review()!.lines} keyExtractor={(_, index) => String(index)} estimatedItemSize={19 * theme().scale}
            style={{ flex: 1, minHeight: 0 }} onViewableItemsChange={(info) => {
              const visible = info.viewableItems.flatMap((token) => typeof token.index === "number" ? [token.index] : []);
              if (!visible.length) return;
              const first = Math.min(...visible), entries = hunks();
              // Locate the hunk at the viewport without rescanning a large patch.
              let low = 0, high = entries.length;
              while (low < high) {
                const mid = (low + high) >>> 1;
                if (entries[mid] <= first) low = mid + 1;
                else high = mid;
              }
              hunk.set(low - 1);
            }}>
            {(line) => <View style={{ flexDirection: "row", paddingHorizontal: 8, paddingVertical: 2, gap: 8, backgroundColor: line().kind === "hunk" ? theme().palette.panelActive : "transparent" }}>
              <Text style={{ ...text(10, theme().palette.muted), fontFamily: theme().monoFamily, width: 35, textAlign: "right" }}>{line().oldLine ?? ""}</Text>
              <View role="button" accessibilityLabel={line().newLine ? `Open line ${line().newLine}` : "Patch line"} onPress={() => { if (line().newLine) props.onOpen(review()!.file.path, line().newLine!); }}>
                <Text style={{ ...text(10, theme().palette.muted), fontFamily: theme().monoFamily, width: 35, textAlign: "right" }}>{line().newLine ?? ""}</Text>
              </View>
              <Text selectable={true} style={{ ...text(11, line().kind === "add" ? "#3da76d" : line().kind === "remove" ? "#df6870" : line().kind === "hunk" ? theme().palette.cyan : theme().palette.text), fontFamily: theme().monoFamily, flex: 1 }}>{line().text || " "}</Text>
            </View>}
          </List>
        </>}
        {!review() && <Text style={{ ...text(12, theme().palette.muted), padding: 18 }}>{readError() || (loading() || reading() ? "Loading reported patch…" : "No file changes were reported here.")}</Text>}
        {!!readError() && !!selected() && button("Try again", () => retry.set(retry() + 1))}
      </View>
    </View>
  </View>;
}
