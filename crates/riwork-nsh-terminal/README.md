# riwork.terminal

The integrated NSH host registers this component on its shared `Components`
registry before constructing any AppView. Its public props, events and commands
are the frozen schema in [the contracts](../../../docs/nsh-contracts.md#3-riworkterminal-native-component).
Local shell tabs support bracketed paste, file/image attachments, terminal Find,
nested shell sessions and dedicated Vim editor tabs. Remote tabs are Phase 6.

The native view is a layer-backed AppKit `NSTextInputClient`. Ghostty's pinned C
library owns its PTY and renderer. Rust prepares the engine's existing
`SessionManager::attach_exec_command` off the main thread, preserving ordinary
tmux client sizing and existing sessions. Native input and rendering do not
cross JavaScript. No viewport lease is taken and the component never creates,
respawns or kills a session. The app's explicit New Shell/Exit controls use the
engine through `riwork/native`.

## Ownership and teardown

Each component owns an NSView and a callback context. Dropping it invalidates
pending attachment work immediately. A surface being disposed retains its view
and context until all of these steps finish:

1. Hide the surface and disable input on the main thread.
2. A bounded background engine call detaches only the surface's exact tty and
   confirms its absence on this RIWORK_HOME's tmux server.
3. Continue processing Ghostty wakeups until its PTY child exits. An early tty
   absence is provisional: if the child remains alive, query/detach again.
4. Free the surface, app and per-surface config on the main thread, then release
   the retained view/context. After two seconds an unfinished disposal stays
   parked, with asynchronous retries and no blocking wait.

The host keeps the AppKit loop alive through the last pending disposal on quit.
Process-wide claims prevent two views from attaching the same local shell.
Ended or unknown sessions retain their last screen; a failed tmux query never
means `gone`. Unexpected attach-child exits query session liveness off-main and
retry only the existing session, with three attempts per reset cycle.

Hidden views use the existing desktop policy: the three most recently shown
stay warm for ten minutes, others have thirty seconds' grace, pinned views stay,
frozen windows release none, and each pass releases at most two surfaces.
The scheduler honors the policy's next deadline, including 250 ms capped passes,
15-second pinned-only waits and five-second frozen waits.

## Configuration and interaction

[BUILD.md](BUILD.md) specifies the source/toolchain hashes, private preparation,
verified C archive, resources and notices. Cargo has no Ghostty/GPUI Rust crate
dependency. The host selects the private theme resources before starting threads.

A single process watcher checks Ghostty config/includes/theme files. Identical
finalized configurations share a bounded native cache; surfaces get owned clones.
App palette changes update the current surface. Returning to Ghostty colors or
changing Ghostty's configuration detaches and reattaches the same session.
Temporary configuration files are unique per request and removed off-main;
revision checks prevent stale theme jobs from winning.

The view routes physical keys, marked-text composition, candidate rectangles,
mouse/drag/scroll, responder editing, backing-scale changes and display IDs to
Ghostty. OSC52 permits unconfirmed plain-text writes; reads and confirmation
requests are denied. Cmd+V and native file/image drops classify files first,
then nonempty text, then a lone image. Delivery uses the engine's per-shell
input lock, copy-mode/input-off checks and bracketed tmux paste without Return.
Unsafe path batches are rejected atomically; spaces, Unicode and shell
metacharacters remain literal arguments. Images are private PNG files owned by
the tab across surface release, removed on failure or final tab disposal.
Startup cleanup removes only image directories whose owning process is proven gone.
Frozen views stop drawing and receiving input while retaining their last Metal
frame in the AppView layer tree.

Local link detection uses the engine's tmux captures, Unicode/wrapped-line/OSC8
parser and path resolver. Hover underlines and eligible Command-clicks stay
native; `linkOpen` tells the app which URL/file/folder was resolved. URL/file
opening uses host validation. Command-Shift-click opens/reuses a project Vim
session, as does Open File in Vim in the View/pane menu. Vim's normal save
semantics apply. New Subshell records its explicit parent and uses a separate
tmux session, so output and exit stay independent. Cmd+F shows a per-terminal
Find bar; Cmd+G/Shift-Cmd+G navigate native Ghostty matches. Hover answers
use the bounded no-watch cache; physical display migration and real Japanese
input-source candidate UI require hardware/manual coverage beyond this fixture.

## Reproduce isolated verification

Normal builds have no automation transport. The `terminal-fixture` Cargo feature
adds a file-command transport that requires an explicit marker inside a random
`/tmp` RIWORK_HOME. The runner creates and removes only its own tmux sessions and
uses the production TSX bundle, real NSH tab mount/unmount and normal host close/
quit paths. Native event probes call the same view handlers; they never send
system-wide input. Cua is used only for read-only observation of the printed PID.

```sh
cargo build -p riwork-desktop --features terminal-fixture --release --locked
"$NSH" build apps/riwork --out /tmp/riwork-terminal-bundle
python3 crates/riwork-desktop/verification/phase2b_terminal_extras.py \
  --binary target/release/riwork --bundle /tmp/riwork-terminal-bundle/bundle.js \
  --cycles 20 --linger-seconds 120
```

The runner records exact argv/environment, binary/bundle hashes, input bytes,
resize/theme readbacks, per-cycle session/pane identities, attach descendants,
native detach/child-exit/free ordering and independent main-loop heartbeats.
The extras fixture additionally checks raw paste bytes and tmux captures,
Unicode/metacharacter shell arguments, private pasteboard priorities, image
lifecycle, nested shell exit isolation, Vim save/reuse and native Find results.
The final report gives the executed commands and retained evidence.
Use the standard release profile for native latency acceptance; debug Objective-C
signature verification adds substantial work to NSH view updates.
