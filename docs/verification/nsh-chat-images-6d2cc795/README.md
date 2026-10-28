# NSH chat screenshot rendering

Task `6d2cc795-4c4c-4e4d-bf10-6358233b268d`, project
`ed5ee40f-dca6-405b-a58e-128e7685da33`, worktree
`9f64ccc2-ae35-46d0-8a7a-d0c7d02ee91a`, branch `fix/nsh-chat-images`.
Task status remains **in_progress** for orchestrator review.

## Cause and change

The provider adapter retains MCP image content in `Presentation.images`, including
persisted chat events. The NSH projection already exposed item/index references,
and `ChatHandle.imagePreview` could retrieve them. However, ChatTab rendered every
reference as a `▧ Image` control. Its only image element was in a click-opened
overlay, so the conversation never requested or mounted screenshot pixels.

The DTO now distinguishes embedded images from local/network references without
copying their payloads into row headers. Mounted conversation cells request
embedded images through the original chat handle and render an actual NSH
`Image`. The native reader fully decodes raster bytes through Files' bounded,
oriented, downsampled PNG decoder and returns intrinsic dimensions. Cards preserve
aspect ratios, expand on click, show loading/errors, allow retries after read or
render failures, limit concurrent decodes to two, and ignore disposed-cell work.

Local references still require an explicit preview action. Their native reads use
the original transcript identity, registered project/repository/worktree and
orchestrator roots, per-chat attachment scope, and no-follow regular-file walk.
No project selection, HOME, common parent or arbitrary temporary directory becomes
an image scope. Remote links require a deliberate HTTP(S) open; rendering does
not automatically fetch them. File/open routing and persistence schemas remain
unchanged. Existing retained embedded images render when their rows are replayed;
payloads absent from older logs cannot be reconstructed.

## Verification environment

`verification_environment = synthetic/disposable chats and images; private HOME,
RIWORK_HOME, RIWORK_RUNTIME_DIR; headless only; isolated output; no production pairings`.

On this worker, the reproducible environment is
`target/image-fix/env.sh`, with HOME, data, runtime and temporary directories under
`/private/tmp/riwork-img-6d2cc795/`. Initial compile/unit checks also used private
directories under `target/image-fix/`; neither environment used real chat history.
Build output and cloned Cargo caches are under this worktree's `target/`.
Rust 1.99.1, Xcode 27.1, NSH 0.3.0, Zig 0.16 and the prepared Ghostty install are
reused from the supplied preparation directory read-only. Its debug/release and
Cargo caches were cloned before use. Cargo resolution is offline.

Administrative calls must retain the actual RIWORK_HOME; none were necessary.
No live GUI input/capture, orchestrator test, updater/reload, host/provider/CLI
restart, OS reboot, pairing or running-bundle modification was performed.
No Cua call or alternate desktop automation provider was used.

## Targeted checks

After sourcing the private build environment, run:

```sh
cargo test --locked --offline -p riwork-nsh-native --lib chat::
cargo test --locked --offline -p riwork-nsh-native --test chat_bridge
CARGO_NET_OFFLINE=true RIWORK_NSH_CHAT_FIXTURE=1 "$NSH_CLI" test apps/riwork \
  --filter 'Chat images' --trace --junit target/image-fix/chat-images.xml
CARGO_NET_OFFLINE=true "$NSH_CLI" build apps/riwork/ChatImagesMount.fixture.tsx \
  --out target/image-fix/image-mount-ui
RIWORK_CHAT_IMAGE_BUNDLE="$PWD/target/image-fix/image-mount-ui/bundle.js" \
  cargo test --locked --offline -p riwork-nsh-native --test chat_image_mount \
  -- --ignored --nocapture
```

These are windowless tests. `--target macos` is intentionally absent; the TSX
`platform: "macos"` option selects the simulated platform. Disposable Unix sockets
need permission outside the managed sandbox, which refused fixture host binds.
No permission refusal was bypassed.

- Chat unit tests: **85 passed**, one existing ignored test. This includes persisted
  Cua image replay, decoded pixel equality, malformed encoding/truncated images,
  dimension/size limits, missing files, symlinks and scope/identity boundaries.
- Existing chat bridge regressions: **11 passed**, including virtualized 2,200-call
  history, streaming, requests, drafts and interruption behavior.
- ChatImages TSX test: exercises real recorded-provider/native-handle rendering,
  explicit local access, missing and denied file fallbacks, and reopening retained
  images from persisted history.
- `chat_image_mount`: the actual compiled ChatTab's image mount contains **145 PNG
  bytes**, decoded by the test as **12×8 RGBA pixels [200,40,10,255]**, with nonzero
  layout dimensions and a visible malformed-image fallback. It inspects native
  `MountItem::Image.source.data`, not placeholder text or a mocked image component.

Logs, JUnit and build receipts are under `target/image-fix/`. There is **no GUI
screenshot**: headless NSH mounts but does not draw. AppKit rasterization and live
window layout remain unverified under this turn's no-GUI constraint.

## Other checks and review limits

The missing `chat/image_scope/tests.rs` is restored with focused native fixtures.
The existing chat bridge's missing SubmitRequest attachment fields and exhaustive
BodyDto match are minimally updated so it compiles. No broad test refactor was made.

An initial full native-library run reported 171 passes, 11 failures and one ignored
test. One new assertion expected a particular wording for parent traversal; it
was corrected to check refusal, and the targeted suite passes. Seven failures
were disposable host binds refused by the sandbox. The other failures were
unrelated existing registry expectation drift (Dictation/Handoff), Git fixture
creation and a file-watcher timeout. The full library suite is not claimed green.
An initial NSH describe attempted registry access; its offline retry succeeded.
An initial test entry and an image-mount bundle-directory argument were corrected
to the app root and `bundle.js`, respectively.

The separate candidate bundle belongs under this worktree's
`target/release/RiWork.app`; its UI is built from the patch and the bundle is
ad-hoc signed and verified before delivery. Artifact hashes and the final commit
are recorded in `target/image-fix/review.json`. No installation or activation is
part of this change. A separately authorized app activation is needed to exercise
the candidate in live windows; no chat-host or provider restart is required by
the image DTO/renderer change.
